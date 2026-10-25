//! Running programs without a shell: the program is looked up in `PATH`
//! (absolute entries only, never the working directory), arguments are
//! passed as a vector, output is captured with limits, and the process is
//! killed on timeout or cancellation, together with its descendants.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::util::{parse, to_args, truncate};
use crate::{PreparedCall, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

const MAX_ARGS: usize = 64;
const MAX_ARG_CHARS: usize = 4096;
const MAX_STREAM_BYTES: usize = 256 * 1024;
const MAX_TIMEOUT_SECS: u64 = 600;
/// Environment variables JARVIS itself uses, never passed on.
const PRIVATE_ENV: &[&str] = &["LLAMA_API_KEY", "LLAMA_ARG_API_KEY_FILE"];

pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(RunCommand::new())]
}

/// Finds a program in `PATH`. On Windows only `.exe`, `.com`, `.cmd` and
/// `.bat` files are considered (Rust escapes batch-file arguments or
/// refuses unsafe ones).
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let candidates: Vec<String> = if cfg!(windows) {
        if Path::new(name).extension().is_some() {
            vec![name.to_owned()]
        } else {
            [".exe", ".com", ".cmd", ".bat"]
                .iter()
                .map(|ext| format!("{name}{ext}"))
                .collect()
        }
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| {
            candidates
                .iter()
                .map(move |c| dir.join(c))
                .collect::<Vec<_>>()
        })
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    let allowed = ["exe", "com", "cmd", "bat"];
    path.is_file()
        && path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .is_some_and(|e| allowed.contains(&e.as_str()))
}

pub(crate) fn validate_args(args: &[String]) -> Result<(), ToolError> {
    if args.len() > MAX_ARGS {
        return Err(ToolError::InvalidInput(format!(
            "at most {MAX_ARGS} arguments"
        )));
    }
    if args
        .iter()
        .any(|a| a.chars().count() > MAX_ARG_CHARS || a.contains('\0'))
    {
        return Err(ToolError::InvalidInput(
            "an argument is too long or invalid".to_owned(),
        ));
    }
    Ok(())
}

/// `program arg "arg with space"` for display.
pub(crate) fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_owned())
        .chain(args.iter().map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command prepared like every program JARVIS starts: no console window,
/// no inherited stdin, private variables removed.
pub(crate) fn base_command(program: &Path, args: &[String], cwd: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(cwd).stdin(Stdio::null());
    for name in PRIVATE_ENV {
        command.env_remove(name);
    }
    jarvis_platform::current_adapter().prepare_background_command(&mut command);
    command
}

/// Output of a finished program.
pub(crate) struct Captured {
    pub status: std::process::ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Runs to completion with captured output, a timeout and cancellation.
pub(crate) async fn capture(
    command: std::process::Command,
    timeout: Duration,
    ctx: &ToolContext,
) -> Result<Captured, ToolError> {
    let mut command = command;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = jarvis_platform::spawn_owned(command)
        .map_err(|err| ToolError::io("starting the program", err))?;
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    // Readers are owned by this future, so cancellation closes the pipes too.
    let operation = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            async {
                match stdout {
                    Some(s) => read_limited(s).await,
                    None => String::new(),
                }
            },
            async {
                match stderr {
                    Some(s) => read_limited(s).await,
                    None => String::new(),
                }
            },
        );
        Ok(Captured {
            status: status.map_err(|err| ToolError::io("waiting for the program", err))?,
            stdout,
            stderr,
        })
    };
    tokio::select! {
        result = operation => result,
        () = tokio::time::sleep(timeout) => {
            let _ = child.start_kill();
            Err(ToolError::TimedOut(timeout.as_secs()))
        }
        () = ctx.cancel.cancelled() => {
            let _ = child.start_kill();
            Err(ToolError::Cancelled)
        }
    }
}

/// Reads a stream to the end, keeping the first [`MAX_STREAM_BYTES`].
async fn read_limited(mut stream: impl AsyncRead + Unpin) -> String {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut dropped = 0usize;
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STREAM_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(room)]);
                dropped += n.saturating_sub(room);
            }
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if dropped > 0 {
        text.push_str(&format!("\n… [{dropped} more bytes]"));
    }
    text
}

// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug)]
struct RunCommand(ToolSpec);

impl RunCommand {
    fn new() -> Self {
        Self(ToolSpec {
            name: "run_command".to_owned(),
            title: "Run a command".to_owned(),
            description: "Runs a program from the allowed list with arguments (no shell: pipes, redirection and `&&` do not work) in a workspace folder and returns its exit code and output. Always asks the user first.".to_owned(),
            parameters: json!({"type": "object", "required": ["program"], "properties": {
                "program": {"type": "string", "description": "Program name, e.g. git, cargo, npm"},
                "args": {"type": "array", "items": {"type": "string"}},
                "cwd": {"type": "string", "description": "Working folder in the workspace"},
                "timeoutSecs": {"type": "integer", "minimum": 1, "maximum": 600}
            }}),
            capabilities: vec![Capability::ProcessSpawn],
            risk: RiskLevel::High,
            idempotent: false,
            plugin: None,
        })
    }
}

#[async_trait]
impl Tool for RunCommand {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let mut args: RunArgs = parse(input)?;
        validate_args(&args.args)?;
        let cwd = ctx
            .scope
            .resolve(crate::util::optional_path(args.cwd.as_deref()))?;
        if !cwd.is_dir() {
            return Err(ToolError::InvalidInput(format!(
                "{} is not a folder",
                cwd.display()
            )));
        }
        let summary = format!(
            "Run `{}` in {}",
            command_line(&args.program, &args.args),
            cwd.display()
        );
        args.cwd = Some(cwd.display().to_string());
        Ok(PreparedCall {
            action: ActionRequest {
                tool: self.0.name.clone(),
                capabilities: self.0.capabilities.clone(),
                risk: RiskLevel::High,
                summary,
                paths: vec![cwd],
                program: Some(args.program.clone()),
                url: None,
            },
            args: to_args(&args)?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: RunArgs = parse(&call.args)?;
        let cwd = ctx
            .scope
            .ensure_inside(Path::new(args.cwd.as_deref().unwrap_or_default()))?;
        let program = resolve_program(&args.program).ok_or_else(|| {
            ToolError::Failed(format!("\"{}\" was not found in PATH", args.program))
        })?;
        let timeout = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(ctx.limits.command_timeout.as_secs())
                .clamp(1, MAX_TIMEOUT_SECS),
        );
        let captured = capture(base_command(&program, &args.args, &cwd), timeout, ctx).await?;
        let code = captured
            .status
            .code()
            .map_or_else(|| "terminated by a signal".to_owned(), |c| c.to_string());
        let content = format!(
            "exit code: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            captured.stdout.trim_end(),
            captured.stderr.trim_end()
        );
        let line = command_line(&args.program, &args.args);
        if captured.status.success() {
            Ok(ToolOutput {
                summary: format!("`{line}` finished (exit {code})"),
                content: truncate(&content, ctx.limits.max_output_chars),
            })
        } else {
            Err(ToolError::Failed(truncate(
                &format!("`{line}` failed\n{content}"),
                ctx.limits.max_output_chars,
            )))
        }
    }
}

#[cfg(test)]
mod capture_tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use crate::{ProcessRegistry, ToolLimits};
    use jarvis_security::FileScope;

    // Child test binaries let us exercise inherited pipes without a shell or
    // external runtime. The leaf is bounded and exits by itself.
    #[test]
    #[ignore = "subprocess fixture"]
    fn pipe_holder() {
        match std::env::var("JARVIS_PIPE_FIXTURE").as_deref() {
            Ok("leaf") => std::thread::sleep(Duration::from_secs(3)),
            Ok("parent") => {
                let mut command = std::process::Command::new(std::env::current_exe().expect("exe"));
                command
                    .args([
                        "--ignored",
                        "--exact",
                        "command::capture_tests::pipe_holder",
                        "--nocapture",
                    ])
                    .env("JARVIS_PIPE_FIXTURE", "leaf");
                jarvis_platform::current_adapter().prepare_background_command(&mut command);
                let mut child = command.spawn().expect("leaf");
                // The parent exits while the child still owns inherited output.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn inherited_pipes_obey_deadline_and_cancellation() {
        for cancel in [false, true] {
            let ctx = ToolContext::new(
                FileScope::default(),
                ToolLimits::default(),
                Arc::new(ProcessRegistry::default()),
            );
            let mut command = std::process::Command::new(std::env::current_exe().expect("exe"));
            command
                .args([
                    "--ignored",
                    "--exact",
                    "command::capture_tests::pipe_holder",
                    "--nocapture",
                ])
                .env("JARVIS_PIPE_FIXTURE", "parent");
            jarvis_platform::current_adapter().prepare_background_command(&mut command);
            let cancellation = ctx.cancel.clone();
            let trigger = async move {
                if cancel {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    cancellation.cancel();
                }
            };
            let duration = if cancel {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(800)
            };
            let started = std::time::Instant::now();
            let (result, ()) = tokio::join!(capture(command, duration, &ctx), trigger);
            assert!(started.elapsed() < Duration::from_secs(2));
            if cancel {
                assert!(matches!(result, Err(ToolError::Cancelled)));
            } else {
                assert!(matches!(result, Err(ToolError::TimedOut(_))));
            }
        }
    }
}
