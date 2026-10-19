//! Test support (feature `test-support`): a fake `llama-server` served by the
//! test binary itself, so lifecycle tests use real processes without llama.cpp.
//!
//! Usage in a test crate:
//!
//! ```ignore
//! #[test]
//! fn fake_llama_server() {
//!     jarvis_runtime::testing::fake_server_entry();
//! }
//! ```
//!
//! and pass [`fake_command_factory`] to the runtime. The factory re-runs the
//! current test binary with `fake_llama_server --exact`, and the entry point
//! serves a minimal llama.cpp-compatible API when `JARVIS_FAKE_LLAMA` is set.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::process::{CommandFactory, LaunchPlan};
use crate::types::{BinarySource, RuntimeBackend, RuntimeBinary};

pub const FAKE_ENV: &str = "JARVIS_FAKE_LLAMA";
/// Name of the test function that hosts the fake server.
pub const FAKE_TEST_NAME: &str = "fake_llama_server";

/// Behaviours of the fake server.
pub mod mode {
    /// Answers 503 for 0.8 s, then serves normally.
    pub const SLOW: &str = "slow";
    /// Prints an error and exits with code 3 before listening.
    pub const EXIT_EARLY: &str = "exit-early";
    /// Becomes ready, then exits after 1.5 s.
    pub const CRASH_AFTER_READY: &str = "crash-after-ready";
    /// Never reports ready.
    pub const NEVER_READY: &str = "never-ready";
}

/// Command factory that starts the fake server in `mode`.
pub fn fake_command_factory(mode: &'static str) -> CommandFactory {
    Arc::new(move |plan: &LaunchPlan| {
        let mut command = std::process::Command::new(&plan.binary);
        command
            .args([
                FAKE_TEST_NAME,
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(FAKE_ENV, mode)
            .env("FAKE_PORT", plan.port.to_string())
            .env("FAKE_KEY", &plan.api_key)
            .env("FAKE_ALIAS", &plan.model_alias);
        command
    })
}

/// A minimal valid GGUF header (architecture `fake`).
pub fn fake_gguf_bytes() -> Vec<u8> {
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    let key = b"general.architecture";
    out.extend_from_slice(&(key.len() as u64).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&8u32.to_le_bytes());
    let value = b"fake";
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// Registers the current test binary as the installed runtime in `data_dir`.
pub async fn install_fake_runtime(data_dir: &Path, sha256: Option<String>) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let sha256 = match sha256 {
        Some(sha256) => sha256,
        None => crate::download::sha256_file(&exe)
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))?,
    };
    let binary = RuntimeBinary {
        path: exe.display().to_string(),
        version: "version: 0 (fake)".into(),
        backend: RuntimeBackend::Cpu,
        source: BinarySource::UserSelected,
        sha256,
        installed_at_ms: 0,
    };
    let runtime_dir = data_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime_dir).await?;
    let json = serde_json::to_vec(&binary).map_err(std::io::Error::other)?;
    tokio::fs::write(runtime_dir.join("install.json"), json).await
}

/// Entry point of the fake server. Returns immediately unless the test binary
/// was started as a fake server.
#[allow(clippy::expect_used, clippy::print_stderr)]
pub fn fake_server_entry() {
    let Ok(mode) = std::env::var(FAKE_ENV) else {
        return;
    };
    let port: u16 = std::env::var("FAKE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .expect("FAKE_PORT");
    let key = std::env::var("FAKE_KEY").unwrap_or_default();
    let alias = std::env::var("FAKE_ALIAS").unwrap_or_default();
    if mode == mode::EXIT_EARLY {
        eprintln!("llama_model_load: error loading model: fake failure");
        std::process::exit(3);
    }
    let started = Instant::now();
    if mode == mode::CRASH_AFTER_READY {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            std::process::exit(1);
        });
    }
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind fake server");
    for stream in listener.incoming().flatten() {
        let (mode, key, alias) = (mode.clone(), key.clone(), alias.clone());
        std::thread::spawn(move || handle(stream, &mode, &key, &alias, started));
    }
}

fn handle(mut stream: TcpStream, mode: &str, key: &str, alias: &str, started: Instant) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let (mut length, mut auth) = (0usize, String::new());
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                auth = value.trim().to_owned();
            }
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let authorized = auth == format!("Bearer {key}");

    let mut reply = |status: &str, content_type: &str, body: &str| {
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    };
    let loading = mode == mode::NEVER_READY
        || (mode == mode::SLOW && started.elapsed() < Duration::from_millis(800));
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") if loading => reply(
            "503 Service Unavailable",
            "application/json",
            r#"{"status":"loading model"}"#,
        ),
        ("GET", "/health") => reply("200 OK", "application/json", r#"{"status":"ok"}"#),
        _ if !authorized => reply(
            "401 Unauthorized",
            "application/json",
            r#"{"error":{"message":"Invalid API Key"}}"#,
        ),
        ("GET", "/v1/models") => {
            let body =
                format!(r#"{{"object":"list","data":[{{"id":"{alias}","object":"model"}}]}}"#);
            reply("200 OK", "application/json", &body);
        }
        ("POST", "/v1/chat/completions") => reply(
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ),
        _ => reply("404 Not Found", "application/json", "{}"),
    }
}
