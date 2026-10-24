//! Speech recognition with a managed whisper.cpp `whisper-server`.
//!
//! The server has no authentication and allows any web origin (CORS `*`),
//! so it listens on `127.0.0.1` on a random port and every route sits
//! under a random secret path prefix; a web page cannot guess either.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use jarvis_runtime::process::{LogTail, free_port, random_key};
use serde::Deserialize;
use tokio::process::Child;

use crate::error::{Result, VoiceError};
use crate::process::{self, Launcher};
use crate::vad::SAMPLE_RATE;
use crate::wav;

const NAME: &str = "whisper-server";
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(250);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcription {
    pub text: String,
}

#[derive(Debug)]
pub struct WhisperServer {
    child: Child,
    port: u16,
    base: String,
    client: reqwest::Client,
    logs: LogTail,
}

impl WhisperServer {
    /// Starts the server in `work_dir` with `model` (a file inside it) and
    /// waits until the model is loaded.
    pub async fn start(
        launcher: &Launcher,
        work_dir: &Path,
        model: &Path,
        threads: usize,
        adapter: &dyn jarvis_platform::PlatformAdapter,
    ) -> Result<Self> {
        let port = free_port()?;
        let prefix = format!("/{}", &random_key()[..32]);
        // An empty folder for the server's static files.
        let public = work_dir.join("run").join("whisper-public");
        tokio::fs::create_dir_all(&public)
            .await
            .map_err(|err| VoiceError::io("preparing whisper-server", err))?;
        let args: Vec<OsString> = vec![
            "--model".into(),
            process::relative(model, work_dir)?,
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string().into(),
            "--request-path".into(),
            prefix.clone().into(),
            "--public".into(),
            process::relative(&public, work_dir)?,
            "--threads".into(),
            threads.clamp(1, 16).to_string().into(),
        ];
        let logs = LogTail::default();
        let child = process::spawn(
            launcher.command(work_dir, &args),
            adapter,
            &logs,
            NAME,
            false,
            false,
        )?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|err| VoiceError::Engine(err.to_string()))?;
        let mut server = Self {
            child,
            port,
            base: format!("http://127.0.0.1:{port}{prefix}"),
            client,
            logs,
        };
        server.wait_ready().await?;
        Ok(server)
    }

    async fn wait_ready(&mut self) -> Result<()> {
        let started = Instant::now();
        let url = format!("{}/health", self.base);
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|err| VoiceError::io("watching whisper-server", err))?
            {
                return Err(process::exited(NAME, status, &self.logs).await);
            }
            if let Ok(response) = self
                .client
                .get(&url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                && response.status().is_success()
            {
                return Ok(());
            }
            if started.elapsed() > READY_TIMEOUT {
                return Err(VoiceError::Engine(format!(
                    "whisper-server did not load the model within {} seconds",
                    READY_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Transcribes 16 kHz mono audio. `language` is "uk", "en" or "auto".
    pub async fn transcribe(&self, samples: &[f32], language: &str) -> Result<Transcription> {
        #[derive(Deserialize)]
        struct Reply {
            text: Option<String>,
            error: Option<String>,
        }
        let audio = wav::encode_pcm16(samples, SAMPLE_RATE);
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name("speech.wav")
            .mime_str("audio/wav")
            .map_err(|err| VoiceError::Engine(err.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            .text("language", language.to_owned())
            .text("temperature", "0.0")
            .text("audio_ctx", audio_context(samples.len()).to_string());
        let response = self
            .client
            .post(format!("{}/inference", self.base))
            .multipart(form)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    VoiceError::Timeout
                } else {
                    VoiceError::Engine(format!("whisper-server request failed: {err}"))
                }
            })?;
        let status = response.status();
        let reply: Reply = response.json().await.map_err(|err| {
            VoiceError::Engine(format!("unexpected whisper-server reply ({status}): {err}"))
        })?;
        if let Some(error) = reply.error {
            return Err(VoiceError::Engine(format!("whisper-server: {error}")));
        }
        if !status.is_success() {
            return Err(VoiceError::Engine(format!(
                "whisper-server answered {status}"
            )));
        }
        Ok(Transcription {
            text: clean(reply.text.as_deref().unwrap_or_default()),
        })
    }

    /// The loopback port (diagnostics and tests).
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn log_tail(&self) -> Vec<String> {
        self.logs.last(20)
    }

    pub async fn stop(mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

/// Whisper marks non-speech as "[BLANK_AUDIO]", "(music)" and similar; such
/// annotations are not words the user said.
/// Encoder context for a phrase. Whisper encodes a 30-second window
/// (1500 frames) by default; sizing it to the phrase plus 4 s of margin made
/// recognition 3–4 times faster in live tests with almost the same
/// accuracy (less margin garbled words).
fn audio_context(samples: usize) -> u32 {
    const FRAMES_PER_SECOND: f32 = 50.0;
    const MARGIN_SECS: f32 = 4.0;
    let seconds = samples as f32 / SAMPLE_RATE as f32;
    ((seconds + MARGIN_SECS) * FRAMES_PER_SECOND)
        .ceil()
        .min(1500.0) as u32
}

fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encoder_context_follows_the_phrase() {
        assert_eq!(audio_context(0), 200);
        assert_eq!(audio_context(16_000 * 3), 350);
        assert_eq!(audio_context(16_000 * 60), 1500);
    }

    #[test]
    fn annotations_are_removed() {
        assert_eq!(clean(" [BLANK_AUDIO]"), "");
        assert_eq!(clean(" Привіт, (кашель) як справи?"), "Привіт, як справи?");
        assert_eq!(clean("Hello [Music] world"), "Hello world");
    }
}
