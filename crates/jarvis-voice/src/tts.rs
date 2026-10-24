//! Speech synthesis with a long-running Piper process.
//!
//! Piper loads the voice once and then reads one JSON request per line
//! from stdin (UTF-8, unlike its command line on Windows), writes the WAV
//! file named in the request and prints its path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jarvis_runtime::process::LogTail;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::error::{Result, VoiceError};
use crate::process::{self, Launcher};
use crate::wav::{self, Decoded};

const NAME: &str = "Piper";
const SYNTHESIS_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiperOptions {
    /// Speaker of a multi-speaker voice.
    pub speaker: Option<u32>,
    /// 1.0 is the voice's natural pace; larger is faster.
    pub rate: f32,
}

#[derive(Debug)]
pub struct Piper {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    work_dir: PathBuf,
    out_dir: PathBuf,
    speaker: Option<u32>,
    counter: u64,
    logs: LogTail,
}

impl Piper {
    /// Starts Piper in `work_dir` with `model` (the `.onnx` file; its
    /// `.onnx.json` config sits next to it).
    pub async fn start(
        launcher: &Launcher,
        work_dir: &Path,
        model: &Path,
        options: PiperOptions,
        adapter: &dyn jarvis_platform::PlatformAdapter,
    ) -> Result<Self> {
        let out_dir = work_dir.join("run").join("speech");
        tokio::fs::create_dir_all(&out_dir)
            .await
            .map_err(|err| VoiceError::io("preparing Piper", err))?;
        let length_scale = 1.0 / options.rate.clamp(0.5, 2.0);
        let args: Vec<OsString> = vec![
            "--model".into(),
            process::relative(model, work_dir)?,
            "--json-input".into(),
            "--length_scale".into(),
            format!("{length_scale:.3}").into(),
            "--sentence_silence".into(),
            "0.15".into(),
        ];
        let logs = LogTail::default();
        let mut child = process::spawn(
            launcher.command(work_dir, &args),
            adapter,
            &logs,
            NAME,
            true,
            true,
        )?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| VoiceError::Engine("Piper has no output stream".into()))?;
        let mut piper = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            work_dir: work_dir.to_path_buf(),
            out_dir,
            speaker: options.speaker,
            counter: 0,
            logs,
        };
        // Loading errors (a broken model) end the process right away.
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(status) = piper
            .child
            .try_wait()
            .map_err(|err| VoiceError::io("watching Piper", err))?
        {
            return Err(process::exited(NAME, status, &piper.logs).await);
        }
        Ok(piper)
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Speaks `text` into audio.
    pub async fn synthesize(&mut self, text: &str, cancel: &CancellationToken) -> Result<Decoded> {
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(MAX_TEXT_CHARS)
            .collect();
        self.counter += 1;
        let file = self.out_dir.join(format!(
            "{}-{}.wav",
            self.child.id().unwrap_or(0),
            self.counter
        ));
        let relative = process::relative(&file, &self.work_dir)?;
        let mut request = serde_json::json!({
            "text": text.trim(),
            "output_file": relative.to_string_lossy(),
        });
        if let Some(speaker) = self.speaker {
            request["speaker_id"] = speaker.into();
        }
        let mut line = request.to_string();
        line.push('\n');

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| VoiceError::Engine("Piper is stopped".into()))?;
        let written = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        if written.is_err() {
            return Err(self.failure().await);
        }
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let deadline = tokio::time::Instant::now() + SYNTHESIS_TIMEOUT;
        loop {
            let reply = tokio::select! {
                () = cancel.cancelled() => {
                    // The process is mid-sentence; a restart is cheaper than
                    // matching up a late reply.
                    let _ = self.child.start_kill();
                    let _ = tokio::fs::remove_file(&file).await;
                    return Err(VoiceError::Cancelled);
                }
                reply = tokio::time::timeout_at(deadline, self.stdout.next_line()) => reply,
            };
            match reply {
                // Piper prints the path of each file it wrote; anything
                // else is diagnostics.
                Ok(Ok(Some(line))) if line.trim_end().ends_with(&name) => break,
                Ok(Ok(Some(line))) => self.logs.push(line),
                Ok(Ok(None) | Err(_)) => return Err(self.failure().await),
                Err(_) => {
                    let _ = self.child.start_kill();
                    return Err(VoiceError::Timeout);
                }
            }
        }
        let bytes = tokio::fs::read(&file)
            .await
            .map_err(|err| VoiceError::io("reading synthesized speech", err))?;
        let _ = tokio::fs::remove_file(&file).await;
        wav::decode(&bytes)
    }

    async fn failure(&mut self) -> VoiceError {
        match tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(Ok(status)) => process::exited(NAME, status, &self.logs).await,
            _ => VoiceError::Engine(format!(
                "Piper stopped answering. Last output:\n{}",
                self.logs.last(12).join("\n")
            )),
        }
    }

    /// Ends the process: closing stdin lets it exit on its own.
    pub async fn stop(mut self) {
        self.stdin.take();
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
    }
}
