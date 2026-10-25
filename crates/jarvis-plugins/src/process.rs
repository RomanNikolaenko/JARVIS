//! Starting and stopping an MCP server process (stdio transport).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::BufReader;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::error::{PluginError, Result};
use crate::mcp::{ClientInfo, McpClient, ServerDescription, read_line};

const LOG_LINES: usize = 200;
const MAX_LOG_LINE_BYTES: usize = 2000;
/// How long a server may take to exit after its input is closed.
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// Environment variables a server inherits from JARVIS (like the official
/// MCP SDKs); everything else it gets is set explicitly.
#[cfg(windows)]
const INHERITED_ENV: &[&str] = &[
    "APPDATA",
    "ComSpec",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "SystemDrive",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERNAME",
    "USERPROFILE",
    "windir",
];
#[cfg(not(windows))]
const INHERITED_ENV: &[&str] = &[
    "HOME", "LANG", "LC_ALL", "LOGNAME", "PATH", "SHELL", "TERM", "TMPDIR", "USER",
];

/// The inherited part of a server's environment.
pub fn inherited_environment() -> Vec<(String, String)> {
    INHERITED_ENV
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|v| ((*name).to_owned(), v)))
        .collect()
}

/// How to start a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Absolute path of the program.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The complete environment (nothing else is inherited).
    pub env: Vec<(String, String)>,
}

/// The last lines a server wrote to stderr.
#[derive(Debug, Clone, Default)]
pub struct LogTail {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogTail {
    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() == LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn last(&self, count: usize) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines
            .iter()
            .skip(lines.len().saturating_sub(count))
            .cloned()
            .collect()
    }
}

/// Stops a server and the programs it started.
///
/// Unix: the server leads its own process group, so the whole group gets
/// `SIGTERM` and, if that is not enough, `SIGKILL`. Windows: without job
/// objects, a server that does not exit by itself is killed with its process
/// tree (`taskkill /T`); programs left behind by a server that exited on its
/// own cannot be found there.
#[derive(Debug)]
struct Termination {
    pid: Option<u32>,
    client: Arc<McpClient>,
    /// `Some` with a description once the process ended.
    exit: watch::Receiver<Option<String>>,
    kill: CancellationToken,
}

impl Termination {
    async fn wait_exit(&self, limit: Duration) -> bool {
        let mut exit = self.exit.clone();
        tokio::time::timeout(limit, exit.wait_for(Option::is_some))
            .await
            .is_ok()
    }

    /// Closes the server's input (the MCP way to ask it to exit), then
    /// escalates.
    async fn stop(&self) {
        self.client.close();
        let exited = self.wait_exit(EXIT_GRACE).await;
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // Also reaches programs the server left behind.
            signal_group(pid, "TERM").await;
            if exited || self.wait_exit(EXIT_GRACE).await {
                return;
            }
            signal_group(pid, "KILL").await;
        }
        #[cfg(windows)]
        if !exited && let Some(pid) = self.pid {
            kill_tree(pid).await;
        }
        if !exited {
            self.kill.cancel();
            self.wait_exit(EXIT_GRACE).await;
        }
    }
}

/// Sends a signal to a process group (`kill -<signal> -- -<group>`).
#[cfg(unix)]
async fn signal_group(group: u32, signal: &str) {
    let result = tokio::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg("--")
        .arg(format!("-{group}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    if let Err(err) = result {
        tracing::debug!(target: "jarvis_plugins::server", "cannot signal process group {group}: {err}");
    }
}

/// Kills a process and the processes it started.
#[cfg(windows)]
async fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    /// No console window for `taskkill`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("taskkill");
    command
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Err(err) = tokio::process::Command::from(command).status().await {
        tracing::debug!(target: "jarvis_plugins::server", "cannot stop process tree {pid}: {err}");
    }
}

/// A running server and its connection.
#[derive(Debug)]
pub struct ServerProcess {
    description: ServerDescription,
    termination: Termination,
}

impl ServerProcess {
    /// Starts the program without a console window and negotiates the
    /// protocol. The server's stderr goes to `logs`.
    pub async fn start(
        spec: &LaunchSpec,
        adapter: &dyn jarvis_platform::PlatformAdapter,
        info: ClientInfo,
        connect_timeout: Duration,
        logs: &LogTail,
    ) -> Result<Self> {
        let mut command = std::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        adapter.prepare_background_command(&mut command);
        // Its own process group, so programs it starts can be stopped too.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|err| PluginError::io(format!("starting {}", spec.program.display()), err))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(PluginError::Invalid(
                "the server's standard streams are unavailable".into(),
            ));
        };
        forward_stderr(stderr, logs.clone());
        let pid = child.id();

        let client = McpClient::start(stdout, stdin, info);
        let (exit_sender, exit) = watch::channel(None);
        let kill = CancellationToken::new();
        {
            let kill = kill.clone();
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                let status = tokio::select! {
                    status = child.wait() => status,
                    () = kill.cancelled() => {
                        let _ = child.start_kill();
                        child.wait().await
                    }
                };
                let text = match status {
                    Ok(status) => format!("the server exited ({status})"),
                    Err(err) => format!("the server could not be watched: {err}"),
                };
                // Programs the server started may keep its output open.
                client.end(text.clone());
                let _ = exit_sender.send(Some(text));
            });
        }

        let termination = Termination {
            pid,
            client: Arc::clone(&client),
            exit,
            kill,
        };
        match client.negotiate(connect_timeout).await {
            Ok(description) => Ok(Self {
                description,
                termination,
            }),
            Err(err) => {
                termination.stop().await;
                Err(err.into())
            }
        }
    }

    pub fn client(&self) -> &Arc<McpClient> {
        &self.termination.client
    }

    /// The server's process id (on Unix also its process group).
    pub fn pid(&self) -> Option<u32> {
        self.termination.pid
    }

    pub fn description(&self) -> &ServerDescription {
        &self.description
    }

    /// Waits until the process ends and says how.
    pub async fn exited(&self) -> String {
        let mut exit = self.termination.exit.clone();
        let text = exit
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|value| value.clone());
        text.unwrap_or_else(|| "the server exited".to_owned())
    }

    /// Closes the server's input, waits briefly, then stops it and the
    /// programs it started (see [`Termination`]).
    pub async fn shutdown(&self) {
        self.termination.stop().await;
    }
}

fn forward_stderr(stream: tokio::process::ChildStderr, logs: LogTail) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while let Ok(Some(truncated)) = read_line(&mut reader, &mut line, MAX_LOG_LINE_BYTES).await
        {
            let mut text = String::from_utf8_lossy(&line).into_owned();
            if truncated {
                text.push('…');
            }
            tracing::debug!(target: "jarvis_plugins::server", "{text}");
            logs.push(text);
        }
    });
}
