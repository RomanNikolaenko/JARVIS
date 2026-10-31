//! CLI provider tests with real processes.
//!
//! The test binary doubles as a fake `codex` / `claude`: started with the
//! arguments of one of those CLIs it plays that CLI instead of running the
//! tests. What it does is chosen by a `mode:` line in the prompt, and its
//! sign-in status by its file name. libtest would write to stdout, so this
//! file has its own `main`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::type_complexity
)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use jarvis_ai::{
    AiError, AiProvider, CLI_DEFAULT_MODEL, ChatChunk, ChatMessage, ChatRequest, ChatRole, CliKind,
    CliProvider, DataLocality, GenerationOptions, ImageData, ProviderDescriptor, ProviderId,
    ProviderKind,
};
use serde_json::json;

/// File-name marker of a fake CLI that is not signed in.
const SIGNED_OUT: &str = "signed-out";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cli) = fake_cli(&args) {
        cli.run(&args);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let tests: &[(
        &str,
        fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    )] = &[
        ("codex_answers_through_the_cli", || {
            Box::pin(codex_answers_through_the_cli())
        }),
        ("claude_code_streams_its_answer", || {
            Box::pin(claude_code_streams_its_answer())
        }),
        ("cli_failures_are_reported", || {
            Box::pin(cli_failures_are_reported())
        }),
        ("dropping_the_stream_stops_the_cli", || {
            Box::pin(dropping_the_stream_stops_the_cli())
        }),
        ("models_need_a_signed_in_cli", || {
            Box::pin(models_need_a_signed_in_cli())
        }),
    ];
    let filter: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let mut ran = 0;
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let started = Instant::now();
        runtime.block_on(test());
        println!("test {name} ... ok ({} ms)", started.elapsed().as_millis());
        ran += 1;
    }
    println!("\ntest result: ok. {ran} passed");
}

// ------------------------------------------------------------------ fake CLI

#[derive(Clone, Copy)]
enum FakeCli {
    Status,
    Codex,
    Claude,
}

fn fake_cli(args: &[String]) -> Option<FakeCli> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match args.first().map(String::as_str) {
        Some("login" | "auth") if args.get(1).map(String::as_str) == Some("status") => {
            Some(FakeCli::Status)
        }
        Some("exec") if has("--json") => Some(FakeCli::Codex),
        Some("-p") if has("--output-format") => Some(FakeCli::Claude),
        _ => None,
    }
}

impl FakeCli {
    fn run(self, args: &[String]) -> ! {
        if let FakeCli::Status = self {
            let name = std::env::current_exe().unwrap();
            let signed_in = !name.to_string_lossy().contains(SIGNED_OUT);
            std::process::exit(if signed_in { 0 } else { 1 });
        }
        let mut prompt = String::new();
        std::io::stdin().read_to_string(&mut prompt).unwrap();
        let mode = prompt
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("mode: "))
            .unwrap_or("reply")
            .to_owned();
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| CLI_DEFAULT_MODEL.to_owned())
        };
        let mut out = std::io::stdout();
        let mut send = |event: serde_json::Value| {
            writeln!(out, "{event}").unwrap();
            out.flush().unwrap();
        };
        if let Some(pid_file) = mode.strip_prefix("hang ") {
            std::fs::write(pid_file, std::process::id().to_string()).unwrap();
        }
        match (self, mode.as_str()) {
            (FakeCli::Codex, "reply") => {
                let images = args
                    .iter()
                    .filter_map(|a| a.strip_prefix("--image="))
                    .filter(|path| Path::new(path).is_file())
                    .count();
                let summary = format!(
                    "model={} sandbox={} tools-off={} images={images} bytes={}",
                    value("-m"),
                    value("--sandbox"),
                    args.iter().any(|a| a == "features.shell_tool=false"),
                    prompt.len()
                );
                send(json!({"type": "thread.started", "thread_id": "t"}));
                send(json!({"type": "turn.started"}));
                send(
                    json!({"type": "item.completed", "item": {"id": "1", "type": "agent_message", "text": summary}}),
                );
                send(
                    json!({"type": "turn.completed", "usage": {"input_tokens": 10, "output_tokens": 4}}),
                );
            }
            (FakeCli::Codex, "limit") => {
                let message = "You've hit your usage limit. Try again at 4:26 PM.";
                send(json!({"type": "error", "message": message}));
                send(json!({"type": "turn.failed", "error": {"message": message}}));
                std::process::exit(1);
            }
            (FakeCli::Claude, "reply") => {
                let tools = value("--tools");
                send(json!({"type": "system", "subtype": "init"}));
                for text in ["model=", &value("--model"), &format!(" tools=[{tools}]")] {
                    send(
                        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}}),
                    );
                }
                send(
                    json!({"type": "result", "subtype": "success", "is_error": false, "result": "ignored", "usage": {"input_tokens": 7, "output_tokens": 3}}),
                );
            }
            (_, "crash") => {
                eprintln!("boom: the CLI broke");
                std::process::exit(2);
            }
            (_, hang) if hang.starts_with("hang ") => {
                let text = "partial answer";
                match self {
                    FakeCli::Claude => send(
                        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}}),
                    ),
                    _ => send(
                        json!({"type": "item.completed", "item": {"id": "1", "type": "agent_message", "text": text}}),
                    ),
                }
                std::thread::sleep(Duration::from_secs(120));
            }
            _ => {}
        }
        std::process::exit(0);
    }
}

// --------------------------------------------------------------------- tests

fn provider(kind: CliKind, program: &Path) -> CliProvider {
    CliProvider::new(
        ProviderDescriptor {
            id: ProviderId::new("cli"),
            display_name: "CLI".into(),
            kind: ProviderKind::Cloud,
            locality: DataLocality::ThirdPartyCloud,
        },
        kind,
    )
    .with_program(program)
}

fn this_exe() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn request(model: &str, messages: Vec<ChatMessage>) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages,
        options: GenerationOptions::default(),
    }
}

async fn collect(provider: &CliProvider, request: ChatRequest) -> Result<String, AiError> {
    let mut stream = provider.stream_chat(request).await?;
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            ChatChunk::Delta(delta) => text.push_str(&delta),
            ChatChunk::Done { usage, .. } => {
                assert!(usage.is_some_and(|u| u.completion_tokens.is_some()));
                return Ok(text);
            }
        }
    }
    panic!("the stream ended without a result");
}

async fn codex_answers_through_the_cli() {
    let codex = provider(CliKind::Codex, &this_exe());
    let image = ImageData {
        media_type: "image/png".into(),
        base64: "iVBORw0KGgo=".into(),
    };
    // A long history must not block on the pipe.
    let history = "x".repeat(2 * 1024 * 1024);
    let answer = collect(
        &codex,
        request(
            "gpt-test",
            vec![
                ChatMessage::new(ChatRole::System, "Be brief."),
                ChatMessage::new(ChatRole::Assistant, history),
                ChatMessage::user("What is this?").with_images(vec![image]),
            ],
        ),
    )
    .await
    .expect("answer");
    assert!(
        answer.starts_with("model=gpt-test sandbox=read-only tools-off=true images=1 bytes="),
        "{answer}"
    );
    let bytes: usize = answer.rsplit('=').next().unwrap().parse().unwrap();
    assert!(bytes > 2 * 1024 * 1024);

    let answer = collect(
        &codex,
        request(CLI_DEFAULT_MODEL, vec![ChatMessage::user("Hi")]),
    )
    .await
    .expect("answer");
    assert!(answer.starts_with("model=default "), "{answer}");
}

async fn claude_code_streams_its_answer() {
    let claude = provider(CliKind::ClaudeCode, &this_exe());
    let mut stream = claude
        .stream_chat(request("sonnet", vec![ChatMessage::user("Hi")]))
        .await
        .expect("stream");
    let mut deltas = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk.expect("chunk") {
            ChatChunk::Delta(text) => deltas.push(text),
            ChatChunk::Done { .. } => break,
        }
    }
    // Deltas arrive as they are printed; the final result is not repeated.
    assert_eq!(deltas, ["model=", "sonnet", " tools=[]"]);

    let images = vec![ImageData {
        media_type: "image/png".into(),
        base64: "iVBORw0KGgo=".into(),
    }];
    let refused = claude
        .stream_chat(request(
            "sonnet",
            vec![ChatMessage::user("Look").with_images(images)],
        ))
        .await;
    assert!(matches!(refused, Err(AiError::InvalidRequest(_))));
    let option = claude
        .stream_chat(request(
            "--dangerously-skip-permissions",
            vec![ChatMessage::user("Hi")],
        ))
        .await;
    assert!(matches!(option, Err(AiError::ModelNotFound { .. })));
}

async fn cli_failures_are_reported() {
    let codex = provider(CliKind::Codex, &this_exe());
    let limit = collect(
        &codex,
        request(CLI_DEFAULT_MODEL, vec![ChatMessage::user("mode: limit")]),
    )
    .await;
    assert!(
        matches!(&limit, Err(AiError::LimitReached(m)) if m.contains("4:26 PM")),
        "{limit:?}"
    );

    for kind in [CliKind::Codex, CliKind::ClaudeCode] {
        let crash = collect(
            &provider(kind, &this_exe()),
            request(CLI_DEFAULT_MODEL, vec![ChatMessage::user("mode: crash")]),
        )
        .await;
        assert!(
            matches!(&crash, Err(AiError::Unavailable(m)) if m.contains("boom: the CLI broke")),
            "{crash:?}"
        );
    }

    let missing = provider(CliKind::Codex, Path::new("definitely-missing-codex"));
    assert!(matches!(
        missing.health_check().await,
        Err(AiError::Unavailable(_))
    ));
    assert!(matches!(
        missing
            .stream_chat(request(CLI_DEFAULT_MODEL, vec![ChatMessage::user("Hi")]))
            .await,
        Err(AiError::Unavailable(_))
    ));
}

async fn dropping_the_stream_stops_the_cli() {
    for kind in [CliKind::Codex, CliKind::ClaudeCode] {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let cli = provider(kind, &this_exe());
        let mut stream = cli
            .stream_chat(request(
                CLI_DEFAULT_MODEL,
                vec![ChatMessage::user(format!(
                    "mode: hang {}",
                    pid_file.display()
                ))],
            ))
            .await
            .expect("stream");
        let first = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .expect("first chunk");
        assert_eq!(first, Some(Ok(ChatChunk::Delta("partial answer".into()))));
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        assert!(jarvis_platform::process_memory(pid).is_some());

        drop(stream);
        let deadline = Instant::now() + Duration::from_secs(10);
        while jarvis_platform::process_memory(pid).is_some() {
            assert!(Instant::now() < deadline, "the CLI kept running");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn models_need_a_signed_in_cli() {
    let claude = provider(CliKind::ClaudeCode, &this_exe());
    let models = claude.list_models().await.expect("models");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["default", "opus", "sonnet", "haiku", "fable"]);
    assert!(
        models
            .iter()
            .all(|m| m.capabilities.chat && !m.capabilities.tool_calling)
    );

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":[{"slug":"gpt-a","display_name":"GPT A","visibility":"list","context_window":272000,"input_modalities":["text","image"]}]}"#,
    )
    .unwrap();
    let codex = provider(CliKind::Codex, &this_exe()).with_codex_home(home.path());
    let models = codex.list_models().await.expect("models");
    assert_eq!(models.len(), 2);
    assert_eq!(models[1].id, "gpt-a");
    assert_eq!(models[1].context_window, Some(272_000));
    assert!(models[1].capabilities.vision);

    // A copy whose name marks it as signed out.
    let signed_out = this_exe().with_file_name(format!(
        "jarvis-ai-cli-{SIGNED_OUT}{}",
        std::env::consts::EXE_SUFFIX
    ));
    let _ = std::fs::remove_file(&signed_out);
    if std::fs::hard_link(this_exe(), &signed_out).is_err() {
        std::fs::copy(this_exe(), &signed_out).unwrap();
    }
    let result = provider(CliKind::Codex, &signed_out).list_models().await;
    let _ = std::fs::remove_file(&signed_out);
    assert!(
        matches!(&result, Err(AiError::Unavailable(m)) if m.contains("codex login")),
        "{result:?}"
    );
}
