//! The managed `llama-server` process: command line, spawning, log capture
//! and readiness checks.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

use crate::error::{Result, RuntimeError};
use crate::types::PoolingType;

const LOG_LINES: usize = 300;
const HEALTH_POLL: Duration = Duration::from_millis(300);
const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything needed to launch the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub binary: PathBuf,
    pub model_path: PathBuf,
    /// Name the server reports for the model (`--alias`).
    pub model_alias: String,
    pub port: u16,
    pub context_size: u32,
    pub threads: u32,
    pub gpu_layers: u32,
    /// Required bearer token; protects the server from other local programs
    /// and from web pages in the user's browser.
    pub api_key: String,
    /// Set for an embedding server.
    pub embeddings: Option<EmbeddingOptions>,
}

/// Options of an embedding server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingOptions {
    pub pooling: PoolingType,
}

/// Command-line arguments for `llama-server`. Only long-standing options are
/// used, so older and newer builds accept them. The API key is not among
/// them: see [`apply_api_key`].
pub fn server_args(plan: &LaunchPlan) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |flag: &str, value: OsString| {
        args.push(flag.into());
        args.push(value);
    };
    push("--model", plan.model_path.clone().into_os_string());
    push("--host", "127.0.0.1".into());
    push("--port", plan.port.to_string().into());
    push("--ctx-size", plan.context_size.to_string().into());
    push("--threads", plan.threads.to_string().into());
    push("--n-gpu-layers", plan.gpu_layers.to_string().into());
    push("--alias", plan.model_alias.clone().into());
    push("--parallel", "1".into());
    if let Some(embeddings) = plan.embeddings {
        push("--pooling", embeddings.pooling.as_arg().into());
        // Each input must fit into one micro-batch: llama-server rejects
        // longer inputs instead of splitting them.
        push("--batch-size", plan.context_size.to_string().into());
        push("--ubatch-size", plan.context_size.to_string().into());
        args.push("--embedding".into());
    }
    args
}

/// Passes the API key in the environment (`LLAMA_API_KEY`). Command lines
/// are visible to other users (`/proc/<pid>/cmdline`); the environment of a
/// process is not. An inherited key file would add keys, so it is dropped.
pub fn apply_api_key(command: &mut std::process::Command, plan: &LaunchPlan) {
    command
        .env_remove("LLAMA_ARG_API_KEY_FILE")
        .env("LLAMA_API_KEY", &plan.api_key);
}

/// Builds the OS command for a plan. Replaceable for tests.
pub type CommandFactory = Arc<dyn Fn(&LaunchPlan) -> std::process::Command + Send + Sync>;

pub fn default_command_factory() -> CommandFactory {
    Arc::new(|plan: &LaunchPlan| {
        let mut command = std::process::Command::new(&plan.binary);
        command.args(server_args(plan));
        apply_api_key(&mut command, plan);
        if let Some(dir) = plan.binary.parent() {
            command.current_dir(dir);
            // Release builds keep their shared libraries next to the binary.
            if cfg!(target_os = "linux") {
                let mut paths = vec![dir.to_path_buf()];
                if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    command.env("LD_LIBRARY_PATH", joined);
                }
            }
        }
        command
    })
}

/// Ring buffer with the server's recent output.
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
        let skip = lines.len().saturating_sub(count);
        lines.iter().skip(skip).cloned().collect()
    }

    pub fn clear(&self) {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// Spawns the server without a console window, capturing its output.
pub fn spawn(
    mut command: std::process::Command,
    adapter: &dyn jarvis_platform::PlatformAdapter,
    logs: &LogTail,
) -> Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    adapter.prepare_background_command(&mut command);
    let mut command = tokio::process::Command::from(command);
    // Never leave a server running if JARVIS drops the handle.
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| RuntimeError::io("starting llama-server", err))?;

    if let Some(stdout) = child.stdout.take() {
        let logs = logs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "jarvis_runtime::server", "{line}");
                logs.push(line);
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let logs = logs.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "jarvis_runtime::server", "{line}");
                logs.push(line);
            }
        });
    }
    Ok(child)
}

/// Waits until `/health` answers 200. Fails if the process exits, the timeout
/// passes or `cancel` fires.
pub async fn wait_ready(
    client: &reqwest::Client,
    port: u16,
    child: &mut Child,
    timeout: Duration,
    cancel: &CancellationToken,
    logs: &LogTail,
) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/health");
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| RuntimeError::io("watching llama-server", err))?
        {
            // Give the log readers a moment to collect the last lines.
            tokio::time::sleep(Duration::from_millis(150)).await;
            return Err(RuntimeError::StartupFailed {
                status: status.to_string(),
                log: logs.last(15).join("\n"),
            });
        }
        if let Ok(response) = client
            .get(&url)
            .timeout(HEALTH_REQUEST_TIMEOUT)
            .send()
            .await
            && response.status().is_success()
        {
            return Ok(());
        }
        if started.elapsed() > timeout {
            return Err(RuntimeError::StartupTimeout(timeout.as_secs()));
        }
        tokio::select! {
            () = cancel.cancelled() => return Err(RuntimeError::Cancelled),
            () = tokio::time::sleep(HEALTH_POLL) => {}
        }
    }
}

/// Checks that the server rejects requests without the API key.
pub async fn api_key_enforced(client: &reqwest::Client, port: u16) -> bool {
    match client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .timeout(HEALTH_REQUEST_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response.status() == reqwest::StatusCode::UNAUTHORIZED,
        Err(_) => false,
    }
}

/// A free TCP port on the loopback interface.
pub fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|err| RuntimeError::io("finding a free port", err))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|err| RuntimeError::io("finding a free port", err))
}

/// Random per-launch API key.
pub fn random_key() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn plan() -> LaunchPlan {
        LaunchPlan {
            binary: PathBuf::from("/opt/llama/llama-server"),
            model_path: PathBuf::from("/models/q.gguf"),
            model_alias: "q".into(),
            port: 4321,
            context_size: 4096,
            threads: 4,
            gpu_layers: 99,
            api_key: "k".into(),
            embeddings: None,
        }
    }

    fn args(plan: &LaunchPlan) -> Vec<String> {
        server_args(plan)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn value(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    #[test]
    fn server_listens_on_loopback_with_a_key() {
        let args = args(&plan());
        let value = |flag: &str| value(&args, flag);
        assert_eq!(value("--host").as_deref(), Some("127.0.0.1"));
        assert_eq!(value("--port").as_deref(), Some("4321"));
        assert_eq!(value("--ctx-size").as_deref(), Some("4096"));
        assert_eq!(value("--n-gpu-layers").as_deref(), Some("99"));
        assert_eq!(value("--alias").as_deref(), Some("q"));
        assert!(!args.iter().any(|a| a.contains("api-key") || a == "k"));
        assert!(!args.iter().any(|a| a == "--embedding" || a == "--pooling"));
    }

    #[test]
    fn embedding_servers_fit_each_input_into_one_batch() {
        let args = args(&LaunchPlan {
            context_size: 2048,
            embeddings: Some(EmbeddingOptions {
                pooling: PoolingType::Cls,
            }),
            ..plan()
        });
        let value = |flag: &str| value(&args, flag);
        assert!(args.iter().any(|a| a == "--embedding"));
        assert_eq!(value("--pooling").as_deref(), Some("cls"));
        assert_eq!(value("--ctx-size").as_deref(), Some("2048"));
        assert_eq!(value("--batch-size").as_deref(), Some("2048"));
        assert_eq!(value("--ubatch-size").as_deref(), Some("2048"));
        assert_eq!(value("--host").as_deref(), Some("127.0.0.1"));
        assert!(!args.iter().any(|a| a == "k"));
    }

    #[test]
    fn the_api_key_is_passed_in_the_environment() {
        let command = default_command_factory()(&plan());
        assert!(!command.get_args().any(|a| a == "k"));
        let env: Vec<_> = command.get_envs().collect();
        assert!(env.contains(&(OsStr::new("LLAMA_API_KEY"), Some(OsStr::new("k")))));
        assert!(env.contains(&(OsStr::new("LLAMA_ARG_API_KEY_FILE"), None)));
    }

    #[test]
    fn keys_and_ports_are_usable() {
        let a = random_key();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_key());
        assert!(free_port().expect("port") > 0);
    }

    #[test]
    fn log_tail_keeps_the_last_lines() {
        let logs = LogTail::default();
        for i in 0..(LOG_LINES + 5) {
            logs.push(format!("line {i}"));
        }
        let last = logs.last(2);
        assert_eq!(
            last,
            vec![
                format!("line {}", LOG_LINES + 3),
                format!("line {}", LOG_LINES + 4)
            ]
        );
        assert_eq!(logs.last(10_000).len(), LOG_LINES);
    }
}
