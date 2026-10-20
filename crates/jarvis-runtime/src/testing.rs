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

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use crate::process::{CommandFactory, LaunchPlan};
use crate::types::{BinarySource, RuntimeBackend, RuntimeBinary};

pub const FAKE_ENV: &str = "JARVIS_FAKE_LLAMA";
/// Set when the fake plays a server started with `--embedding`.
pub const FAKE_EMBEDDINGS_ENV: &str = "FAKE_EMBEDDINGS";
/// Name of the test function that hosts the fake server.
pub const FAKE_TEST_NAME: &str = "fake_llama_server";
/// Starts the crash of [`mode::CRASH_AFTER_READY`] once.
static CRASH_TIMER: Once = Once::new();
/// Dimensions of the fake embedding vectors.
pub const FAKE_EMBEDDING_DIMS: usize = 16;

/// Words the fake embedding model treats as one meaning each, so tests can
/// tell semantic matches from word matches ("kitten" finds "cat").
const FAKE_CONCEPTS: &[&[&str]] = &[
    &["cat", "cats", "kitten", "kittens", "кіт", "кішка", "кошеня"],
    &["car", "cars", "automobile", "vehicle", "авто", "машина"],
    &["weather", "rain", "sunny", "погода", "дощ"],
];

/// Behaviours of the fake server. A plan with embedding options makes it
/// play a server started with `--embedding` (see [`fake_embedding`]).
pub mod mode {
    /// Answers 503 for 0.8 s, then serves normally.
    pub const SLOW: &str = "slow";
    /// Prints an error and exits with code 3 before listening.
    pub const EXIT_EARLY: &str = "exit-early";
    /// Becomes ready, then exits 1.5 s after the first successful health
    /// check (timed from readiness, so slow test machines do not crash it
    /// while it is still loading).
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
            .env("FAKE_ALIAS", &plan.model_alias);
        if plan.embeddings.is_some() {
            command.env(FAKE_EMBEDDINGS_ENV, "1");
        }
        // The key reaches the fake exactly as it reaches llama-server.
        crate::process::apply_api_key(&mut command, plan);
        command
    })
}

/// A minimal valid GGUF header (architecture `fake`).
pub fn fake_gguf_bytes() -> Vec<u8> {
    fake_gguf(None)
}

/// A minimal GGUF header of an embedding model (CLS pooling, like bge-m3).
pub fn fake_embedding_gguf_bytes() -> Vec<u8> {
    fake_gguf(Some(2))
}

/// A minimal GGUF header, optionally declaring `fake.pooling_type` (the
/// GGUF value: 0 none, 1 mean, 2 CLS, 3 last, 4 rank).
pub fn fake_gguf(pooling_type: Option<u32>) -> Vec<u8> {
    let string = |out: &mut Vec<u8>, s: &[u8]| {
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s);
    };
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&(1 + u64::from(pooling_type.is_some())).to_le_bytes());
    string(&mut out, b"general.architecture");
    out.extend_from_slice(&8u32.to_le_bytes());
    string(&mut out, b"fake");
    if let Some(pooling_type) = pooling_type {
        string(&mut out, b"fake.pooling_type");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&pooling_type.to_le_bytes());
    }
    out
}

/// Deterministic stand-in for an embedding model: words of one
/// [`FAKE_CONCEPTS`] group share a dimension (weight 3), other words are
/// hashed into the remaining dimensions (weight 0.5).
pub fn fake_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; FAKE_EMBEDDING_DIMS];
    let hashed = FAKE_EMBEDDING_DIMS - FAKE_CONCEPTS.len();
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = word.to_lowercase();
        match FAKE_CONCEPTS
            .iter()
            .position(|group| group.contains(&word.as_str()))
        {
            Some(concept) => vector[concept] += 3.0,
            None => {
                let hash = word.bytes().fold(17usize, |h, b| {
                    h.wrapping_mul(31).wrapping_add(usize::from(b))
                });
                vector[FAKE_CONCEPTS.len() + hash % hashed] += 0.5;
            }
        }
    }
    vector
}

/// Registers a copy of the current test binary as the installed runtime in
/// `data_dir` and returns its path. The copy sits alone in `data_dir/engine`
/// like a real build folder (the test binary's own folder is full of
/// unrelated libraries).
pub async fn install_fake_runtime(
    data_dir: &Path,
    sha256: Option<String>,
) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let engine_dir = data_dir.join("engine");
    tokio::fs::create_dir_all(&engine_dir).await?;
    let binary = engine_dir.join(exe.file_name().unwrap_or(OsStr::new("fake-llama-server")));
    if !binary.exists() && std::fs::hard_link(&exe, &binary).is_err() {
        tokio::fs::copy(&exe, &binary).await?;
    }
    let to_io = |err: crate::RuntimeError| std::io::Error::other(err.to_string());
    let sha256 = match sha256 {
        Some(sha256) => sha256,
        None => crate::download::sha256_file(&binary).await.map_err(to_io)?,
    };
    let record = RuntimeBinary {
        path: binary.display().to_string(),
        version: "version: 0 (fake)".into(),
        backend: RuntimeBackend::Cpu,
        source: BinarySource::UserSelected,
        sha256,
        libraries: crate::installer::library_digests(&binary)
            .await
            .map_err(to_io)?,
        installed_at_ms: 0,
    };
    let runtime_dir = data_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime_dir).await?;
    let json = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    tokio::fs::write(runtime_dir.join("install.json"), json).await?;
    Ok(binary)
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
    let key = std::env::var("LLAMA_API_KEY").unwrap_or_default();
    let alias = std::env::var("FAKE_ALIAS").unwrap_or_default();
    let embeddings = std::env::var_os(FAKE_EMBEDDINGS_ENV).is_some();
    if mode == mode::EXIT_EARLY {
        eprintln!("llama_model_load: error loading model: fake failure");
        std::process::exit(3);
    }
    let started = Instant::now();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind fake server");
    for stream in listener.incoming().flatten() {
        let (mode, key, alias) = (mode.clone(), key.clone(), alias.clone());
        std::thread::spawn(move || handle(stream, &mode, &key, &alias, embeddings, started));
    }
}

fn handle(
    mut stream: TcpStream,
    mode: &str,
    key: &str,
    alias: &str,
    embeddings: bool,
    started: Instant,
) {
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
        ("GET", "/health") => {
            if mode == mode::CRASH_AFTER_READY {
                CRASH_TIMER.call_once(|| {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(1500));
                        std::process::exit(1);
                    });
                });
            }
            reply("200 OK", "application/json", r#"{"status":"ok"}"#);
        }
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
        // Like llama-server: `--embedding` restricts the server to embeddings.
        ("POST", "/v1/chat/completions") if embeddings => reply(
            "501 Not Implemented",
            "application/json",
            r#"{"error":{"code":501,"message":"This server only computes embeddings","type":"not_supported_error"}}"#,
        ),
        ("POST", "/v1/chat/completions") => reply(
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ),
        ("POST", "/v1/embeddings") if !embeddings => reply(
            "501 Not Implemented",
            "application/json",
            r#"{"error":{"code":501,"message":"This server does not support embeddings. Start it with `--embeddings`","type":"not_supported_error"}}"#,
        ),
        ("POST", "/v1/embeddings") => match embeddings_reply(&body, alias) {
            Some(json) => reply("200 OK", "application/json", &json),
            None => reply(
                "400 Bad Request",
                "application/json",
                r#"{"error":{"code":400,"message":"\"input\" must be provided","type":"invalid_request_error"}}"#,
            ),
        },
        _ => reply("404 Not Found", "application/json", "{}"),
    }
}

/// OpenAI-style embeddings response for a request body.
fn embeddings_reply(body: &[u8], alias: &str) -> Option<String> {
    let request: serde_json::Value = serde_json::from_slice(body).ok()?;
    let inputs: Vec<String> = match request.get("input")? {
        serde_json::Value::String(text) => vec![text.clone()],
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
        _ => return None,
    };
    let data: Vec<serde_json::Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, text)| {
            serde_json::json!({
                "object": "embedding",
                "index": index,
                "embedding": fake_embedding(text),
            })
        })
        .collect();
    Some(serde_json::json!({ "object": "list", "model": alias, "data": data }).to_string())
}
