//! whisper-server and Piper process handling, against fakes served by this
//! test binary (real processes, no engines needed).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jarvis_voice::process::Launcher;
use jarvis_voice::stt::WhisperServer;
use jarvis_voice::tts::{Piper, PiperOptions};
use jarvis_voice::{VoiceError, wav};
use tokio_util::sync::CancellationToken;

const FAKE_ENV: &str = "JARVIS_FAKE_VOICE";

/// A launcher that re-runs this test binary as the fake `engine`. The
/// engine's own arguments come after `--`, where the test harness takes
/// them as name filters that match nothing else.
fn fake(engine: &str, test: &str) -> Launcher {
    let mut launcher = Launcher::new(std::env::current_exe().expect("test binary"));
    launcher.prefix_args = vec![
        test.into(),
        "--exact".into(),
        "--nocapture".into(),
        "--".into(),
    ];
    launcher.envs = vec![(FAKE_ENV.into(), engine.into())];
    launcher
}

fn engine_args() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    let start = args
        .iter()
        .position(|a| a == "--")
        .map_or(args.len(), |i| i + 1);
    args[start..].to_vec()
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

// ----- fake whisper-server -----

#[test]
fn fake_whisper_server() {
    if std::env::var(FAKE_ENV).as_deref() != Ok("whisper") {
        return;
    }
    let args = engine_args();
    let host = arg(&args, "--host").expect("--host");
    assert_eq!(host, "127.0.0.1", "the server must stay on loopback");
    let port: u16 = arg(&args, "--port").expect("--port").parse().expect("port");
    let prefix = arg(&args, "--request-path").expect("--request-path");
    assert!(
        prefix.starts_with('/') && prefix.len() >= 33,
        "weak request path {prefix}"
    );
    let model = arg(&args, "--model").expect("--model");
    assert!(
        Path::new(&model).is_relative(),
        "model path must be relative: {model}"
    );
    if std::fs::metadata(&model).is_err() {
        eprintln!("error: failed to open model '{model}'");
        std::process::exit(2);
    }
    let public = arg(&args, "--public").expect("--public");
    assert!(Path::new(&public).is_dir(), "public dir {public}");
    let started = Instant::now();
    let listener = TcpListener::bind((host.as_str(), port)).expect("bind");
    for stream in listener.incoming().flatten() {
        let loaded = started.elapsed() > Duration::from_millis(300);
        serve_whisper(stream, &prefix, loaded);
    }
}

fn serve_whisper(mut stream: TcpStream, prefix: &str, loaded: bool) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, reply) = if method == "GET" && path == format!("{prefix}/health") {
        if loaded {
            ("200 OK", r#"{"status":"ok"}"#.to_owned())
        } else {
            (
                "503 Service Unavailable",
                r#"{"status":"loading model"}"#.to_owned(),
            )
        }
    } else if method == "POST" && path == format!("{prefix}/inference") {
        let text = String::from_utf8_lossy(&body);
        let language = text
            .split("name=\"language\"")
            .nth(1)
            .and_then(|rest| rest.split("\r\n\r\n").nth(1))
            .and_then(|value| value.split("\r\n").next())
            .unwrap_or("");
        if !body.windows(4).any(|w| w == b"RIFF") {
            (
                "400 Bad Request",
                r#"{"error":"no 'file' field in the request"}"#.to_owned(),
            )
        } else {
            let said = if language == "uk" {
                " [BLANK_AUDIO] Джарвіс, котра година?"
            } else {
                " Jarvis, what time is it?"
            };
            ("200 OK", serde_json::json!({ "text": said }).to_string())
        }
    } else {
        ("404 Not Found", String::new())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

// ----- fake Piper -----

#[test]
fn fake_piper() {
    if std::env::var(FAKE_ENV).as_deref() != Ok("piper") {
        return;
    }
    let args = engine_args();
    let model = arg(&args, "--model").expect("--model");
    assert!(args.iter().any(|a| a == "--json-input"));
    if std::fs::metadata(&model).is_err() {
        eprintln!("Unable to load voice from {model}");
        std::process::exit(1);
    }
    let length_scale: f32 = arg(&args, "--length_scale")
        .expect("--length_scale")
        .parse()
        .expect("scale");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.expect("line");
        let request: serde_json::Value = serde_json::from_str(&line).expect("json request");
        let text = request["text"].as_str().expect("text").to_owned();
        if text.contains("CRASH") {
            eprintln!("fake piper: terminate called after throwing an instance");
            std::process::exit(3);
        }
        let output = request["output_file"].as_str().expect("output_file");
        let speaker = request
            .get("speaker_id")
            .map(ToString::to_string)
            .unwrap_or_default();
        // 50 ms per character at 22.05 kHz, longer when slower.
        let count = (text.chars().count() as f32 * 0.05 * 22_050.0 * length_scale) as usize;
        let samples: Vec<f32> = (0..count)
            .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / 22_050.0).sin() * 0.3)
            .collect();
        std::fs::write(output, wav::encode_pcm16(&samples, 22_050)).expect("write wav");
        std::fs::write(format!("{output}.txt"), format!("{text}|{speaker}")).expect("write text");
        writeln!(out, "{output}").expect("stdout");
        out.flush().expect("flush");
    }
}

// ----- tests -----

fn adapter() -> Box<dyn jarvis_platform::PlatformAdapter> {
    jarvis_platform::current_adapter()
}

fn work_dir() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let model_dir = temp.path().join("model-a");
    std::fs::create_dir_all(&model_dir).expect("dir");
    let model = model_dir.join("model.bin");
    std::fs::write(&model, b"model").expect("model");
    (temp, model)
}

#[tokio::test(flavor = "multi_thread")]
async fn whisper_server_runs_on_loopback_behind_a_secret_path() {
    let (temp, model) = work_dir();
    let server = WhisperServer::start(
        &fake("whisper", "fake_whisper_server"),
        temp.path(),
        &model,
        2,
        adapter().as_ref(),
    )
    .await
    .expect("start");

    let speech = vec![0.1f32; 16_000];
    let uk = server.transcribe(&speech, "uk").await.expect("transcribe");
    // Annotations such as [BLANK_AUDIO] are not words.
    assert_eq!(uk.text, "Джарвіс, котра година?");
    let en = server.transcribe(&speech, "en").await.expect("transcribe");
    assert_eq!(en.text, "Jarvis, what time is it?");

    // Without the secret prefix there is nothing to reach.
    let port = server.port();
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    for path in ["/health", "/inference", "/load"] {
        let status = client
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .await
            .expect("request")
            .status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{path}");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_whisper_server_that_cannot_load_its_model_reports_why() {
    let (temp, model) = work_dir();
    std::fs::remove_file(&model).expect("remove");
    let err = WhisperServer::start(
        &fake("whisper", "fake_whisper_server"),
        temp.path(),
        &model,
        2,
        adapter().as_ref(),
    )
    .await
    .expect_err("must fail");
    let message = err.to_string();
    assert!(message.contains("failed to open model"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn piper_speaks_ukrainian_text_through_stdin() {
    let (temp, model) = work_dir();
    let options = PiperOptions {
        speaker: Some(1),
        rate: 1.0,
    };
    let mut piper = Piper::start(
        &fake("piper", "fake_piper"),
        temp.path(),
        &model,
        options,
        adapter().as_ref(),
    )
    .await
    .expect("start");
    let cancel = CancellationToken::new();
    let text = "Добрий день! Ґудзик, їжак, «лапки» — і 42.";
    let audio = piper.synthesize(text, &cancel).await.expect("synthesize");
    assert_eq!(audio.sample_rate, 22_050);
    assert!(
        audio.samples.len() > 22_050,
        "{} samples",
        audio.samples.len()
    );

    // The fake kept what it received: the text arrived intact as UTF-8.
    let speech_dir = temp.path().join("run").join("speech");
    let received: Vec<String> = std::fs::read_dir(&speech_dir)
        .expect("dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "txt"))
        .map(|e| std::fs::read_to_string(e.path()).expect("read"))
        .collect();
    assert_eq!(received, vec![format!("{text}|1")]);
    // The WAV itself is removed after reading.
    assert!(
        !std::fs::read_dir(&speech_dir)
            .expect("dir")
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "wav"))
    );

    // Control characters cannot break the line protocol.
    let audio = piper
        .synthesize("перший\nдругий\r\tтретій", &cancel)
        .await
        .expect("synthesize");
    assert!(!audio.samples.is_empty());
    assert!(piper.is_running());
    piper.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_piper_crash_is_reported_with_its_output() {
    let (temp, model) = work_dir();
    let options = PiperOptions {
        speaker: None,
        rate: 1.5,
    };
    let mut piper = Piper::start(
        &fake("piper", "fake_piper"),
        temp.path(),
        &model,
        options,
        adapter().as_ref(),
    )
    .await
    .expect("start");
    let cancel = CancellationToken::new();
    piper
        .synthesize("Привіт", &cancel)
        .await
        .expect("first sentence");
    let err = piper
        .synthesize("CRASH now", &cancel)
        .await
        .expect_err("crash");
    let message = err.to_string();
    assert!(message.contains("terminate called"), "{message}");
    assert!(!piper.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_voice_fails_at_start() {
    let (temp, model) = work_dir();
    std::fs::remove_file(&model).expect("remove");
    let options = PiperOptions {
        speaker: None,
        rate: 1.0,
    };
    let result = Piper::start(
        &fake("piper", "fake_piper"),
        temp.path(),
        &model,
        options,
        adapter().as_ref(),
    )
    .await;
    // Exiting may take longer than the startup check on a slow machine;
    // then the first sentence reports it.
    match result {
        Err(err) => assert!(err.to_string().contains("Unable to load voice"), "{err}"),
        Ok(mut piper) => {
            let err = piper
                .synthesize("Привіт", &CancellationToken::new())
                .await
                .expect_err("must fail");
            assert!(matches!(err, VoiceError::Engine(_)), "{err}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_synthesis_stops_at_once() {
    let (temp, model) = work_dir();
    let options = PiperOptions {
        speaker: None,
        rate: 1.0,
    };
    let mut piper = Piper::start(
        &fake("piper", "fake_piper"),
        temp.path(),
        &model,
        options,
        adapter().as_ref(),
    )
    .await
    .expect("start");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = piper
        .synthesize("Довге речення", &cancel)
        .await
        .expect_err("cancelled");
    assert!(matches!(err, VoiceError::Cancelled), "{err}");
}
