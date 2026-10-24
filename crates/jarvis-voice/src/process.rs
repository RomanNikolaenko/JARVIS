//! Starting the voice engine processes (whisper-server, Piper).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use jarvis_runtime::process::LogTail;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

use crate::error::{Result, VoiceError};

/// How to start an engine program. Tests point it at a fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    pub program: PathBuf,
    /// Arguments placed before the engine's own.
    pub prefix_args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
}

impl Launcher {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
            envs: Vec::new(),
        }
    }

    /// The command, run in `cwd`. Engine paths are passed relative to it:
    /// both engines read their arguments in the ANSI code page on Windows,
    /// which cannot represent every user profile path.
    pub fn command(&self, cwd: &Path, args: &[OsString]) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.prefix_args).args(args).current_dir(cwd);
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        // Release builds keep their shared libraries next to the program.
        if cfg!(target_os = "linux")
            && let Some(dir) = self.program.parent()
        {
            let mut paths = vec![dir.to_path_buf()];
            if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("LD_LIBRARY_PATH", joined);
            }
        }
        command
    }
}

/// Spawns an engine without a console window. Its stderr (and stdout,
/// unless `keep_stdout`) goes to `logs`; stdin is piped when `stdin` is set.
pub fn spawn(
    mut command: std::process::Command,
    adapter: &dyn jarvis_platform::PlatformAdapter,
    logs: &LogTail,
    name: &str,
    stdin: bool,
    keep_stdout: bool,
) -> Result<Child> {
    command
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    adapter.prepare_background_command(&mut command);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| VoiceError::io(format!("starting {name}"), err))?;
    if !keep_stdout && let Some(stdout) = child.stdout.take() {
        forward(stdout, logs.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward(stderr, logs.clone());
    }
    Ok(child)
}

fn forward(stream: impl tokio::io::AsyncRead + Unpin + Send + 'static, logs: LogTail) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "jarvis_voice::engine", "{line}");
            logs.push(line);
        }
    });
}

/// The error for a process that exited, with its last output.
pub async fn exited(name: &str, status: std::process::ExitStatus, logs: &LogTail) -> VoiceError {
    // Give the log readers a moment to collect the last lines.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let log = logs.last(12).join("\n");
    VoiceError::Engine(format!("{name} exited ({status}). Last output:\n{log}"))
}

/// Path of `path` relative to `base`, with `/` separators, for engines run
/// in `base`.
pub fn relative(path: &Path, base: &Path) -> Result<OsString> {
    let relative = path.strip_prefix(base).map_err(|_| {
        VoiceError::Invalid(format!("{} is outside {}", path.display(), base.display()))
    })?;
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok(OsString::from(parts.join("/")))
}
