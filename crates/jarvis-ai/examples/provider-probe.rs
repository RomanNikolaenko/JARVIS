//! Talks to a model server through JARVIS's own provider code and reports
//! what works: health, models and their capabilities, a streamed reply with
//! the time to the first token, cancellation in the middle of a reply, and
//! embeddings. For checking real servers (Ollama, LM Studio, llama.cpp
//! server, vLLM, ...).
//!
//! ```text
//! cargo run -p jarvis-ai --example provider-probe -- <ollama|openai-compatible> <base url> [--model <id>] [--embed-model <id>]
//! cargo run -p jarvis-ai --example provider-probe -- ollama http://127.0.0.1:11434 --model qwen2.5:3b
//! cargo run -p jarvis-ai --example provider-probe -- openai-compatible http://127.0.0.1:1234/v1
//! ```
//!
//! Cancellation is checked the way JARVIS cancels: the stream is dropped
//! after a few words, then a short request follows at once. A server that
//! keeps generating the abandoned answer delays that request (llama.cpp with
//! one slot, Ollama with one parallel request).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures::StreamExt;
use jarvis_ai::{
    AiProvider, ChatChunk, ChatMessage, ChatRequest, ChatRole, EmbeddingRequest, FinishReason,
    GenerationOptions, HttpProviderFactory, ProviderBackend, ProviderConfig, ProviderFactory,
    ProviderId,
};

const SHORT_PROMPT: &str = "Count from 1 to 5, one number per line, nothing else.";
const LONG_PROMPT: &str = "Write a detailed story of about 400 words about a lighthouse keeper.";
/// Deltas read before the long answer is abandoned.
const DELTAS_BEFORE_CANCEL: usize = 8;

struct Report {
    failures: usize,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: &str) {
        println!(
            "{} {name}{}",
            if ok { "ok  " } else { "FAIL" },
            if detail.is_empty() {
                String::new()
            } else {
                format!(" — {detail}")
            }
        );
        if !ok {
            self.failures += 1;
        }
    }
}

fn request(model: &str, prompt: &str, max_tokens: u32) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![
            ChatMessage::new(ChatRole::System, "You are a concise assistant."),
            ChatMessage::user(prompt),
        ],
        options: GenerationOptions {
            temperature: Some(0.0),
            max_output_tokens: Some(max_tokens),
        },
    }
}

/// A streamed answer: text, time to the first delta, total time, finish reason.
async fn streamed(
    provider: &dyn AiProvider,
    request: ChatRequest,
) -> Result<
    (
        String,
        Duration,
        Duration,
        FinishReason,
        Option<jarvis_ai::TokenUsage>,
    ),
    String,
> {
    let started = Instant::now();
    let mut stream = provider
        .stream_chat(request)
        .await
        .map_err(|err| err.to_string())?;
    let mut text = String::new();
    let mut first = None;
    let mut finish = FinishReason::Stop;
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        match chunk.map_err(|err| err.to_string())? {
            ChatChunk::Delta(delta) => {
                first.get_or_insert_with(|| started.elapsed());
                text.push_str(&delta);
            }
            ChatChunk::Done {
                finish_reason,
                usage: reported,
            } => {
                finish = finish_reason;
                usage = reported;
            }
        }
    }
    Ok((
        text,
        first.unwrap_or_default(),
        started.elapsed(),
        finish,
        usage,
    ))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let option = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let (backend, base_url) = match args.as_slice() {
        [backend, url, ..] if backend == "ollama" => (ProviderBackend::Ollama, url.clone()),
        [backend, url, ..] if backend == "openai-compatible" => {
            (ProviderBackend::OpenAiCompatible, url.clone())
        }
        _ => {
            eprintln!(
                "usage: provider-probe <ollama|openai-compatible> <base url> [--model <id>] [--embed-model <id>]"
            );
            return ExitCode::from(2);
        }
    };
    let config = ProviderConfig {
        id: ProviderId::new("probe"),
        backend,
        display_name: "Probe".to_owned(),
        base_url,
        enabled: true,
    };
    let provider = match HttpProviderFactory::new().create(&config, None) {
        Ok(provider) => provider,
        Err(err) => {
            eprintln!("cannot create the provider: {err}");
            return ExitCode::from(2);
        }
    };
    let mut report = Report { failures: 0 };
    println!(
        "provider: {:?} at {} ({:?})",
        backend,
        config.base_url,
        provider.descriptor().locality
    );

    let health = provider.health_check().await;
    report.check(
        "health check",
        health.is_ok(),
        &health.err().map(|e| e.to_string()).unwrap_or_default(),
    );

    let models = match provider.list_models().await {
        Ok(models) => models,
        Err(err) => {
            report.check("list models", false, &err.to_string());
            return ExitCode::FAILURE;
        }
    };
    report.check(
        "list models",
        !models.is_empty(),
        &format!("{} models", models.len()),
    );
    for model in &models {
        let c = model.capabilities;
        println!(
            "     {} (context {}): chat {} streaming {} embeddings {} tools {} vision {}",
            model.id,
            model
                .context_window
                .map_or("?".to_owned(), |n| n.to_string()),
            c.chat,
            c.streaming,
            c.embeddings,
            c.tool_calling,
            c.vision
        );
    }

    let Some(model) = option("--model").or_else(|| {
        models
            .iter()
            .find(|m| m.capabilities.chat)
            .map(|m| m.id.clone())
    }) else {
        report.check("a chat model", false, "none listed; pass --model");
        return ExitCode::FAILURE;
    };
    match provider.capabilities(&model).await {
        Ok(c) => report.check(
            &format!("capabilities of {model}"),
            c.chat,
            &format!(
                "chat {} streaming {} tools {} vision {}",
                c.chat, c.streaming, c.tool_calling, c.vision
            ),
        ),
        Err(err) => report.check(&format!("capabilities of {model}"), false, &err.to_string()),
    }

    // A streamed answer.
    match streamed(provider.as_ref(), request(&model, SHORT_PROMPT, 64)).await {
        Ok((text, first, total, finish, usage)) => {
            let numbers = (1..=5).all(|n| text.contains(&n.to_string()));
            report.check(
                "streamed reply",
                numbers && finish == FinishReason::Stop,
                &format!(
                    "first token after {:.1} s, done after {:.1} s, {finish:?}, usage {usage:?}: {:?}",
                    first.as_secs_f32(),
                    total.as_secs_f32(),
                    text.trim()
                ),
            );
        }
        Err(err) => report.check("streamed reply", false, &err),
    }

    // A reply cut short by the output limit.
    match streamed(provider.as_ref(), request(&model, LONG_PROMPT, 16)).await {
        Ok((_, _, _, finish, _)) => report.check(
            "output limit reported",
            finish == FinishReason::Length,
            &format!("{finish:?}"),
        ),
        Err(err) => report.check("output limit reported", false, &err),
    }

    // Cancellation: drop a long answer, then a short request must not wait for it.
    match provider
        .stream_chat(request(&model, LONG_PROMPT, 600))
        .await
    {
        Ok(mut stream) => {
            let mut deltas = 0;
            while deltas < DELTAS_BEFORE_CANCEL {
                match stream.next().await {
                    Some(Ok(ChatChunk::Delta(_))) => deltas += 1,
                    Some(Ok(ChatChunk::Done { .. })) | None => break,
                    Some(Err(err)) => {
                        report.check("cancellation", false, &err.to_string());
                        break;
                    }
                }
            }
            drop(stream);
            let reference =
                match streamed(provider.as_ref(), request(&model, SHORT_PROMPT, 64)).await {
                    Ok((_, first, _, _, _)) => first,
                    Err(err) => {
                        report.check("request after cancellation", false, &err);
                        Duration::MAX
                    }
                };
            // Generating ~600 tokens on a CPU takes far longer than 15 s.
            report.check(
                "request right after cancellation is not held up",
                reference < Duration::from_secs(15),
                &format!("first token after {:.1} s", reference.as_secs_f32()),
            );
        }
        Err(err) => report.check("cancellation", false, &err.to_string()),
    }

    // Embeddings.
    let embed_model = option("--embed-model").or_else(|| {
        models
            .iter()
            .find(|m| m.capabilities.embeddings)
            .map(|m| m.id.clone())
    });
    match embed_model {
        Some(embed_model) => {
            let result = provider
                .embeddings(EmbeddingRequest {
                    model: embed_model.clone(),
                    input: vec!["local first".to_owned(), "privacy first".to_owned()],
                })
                .await;
            match result {
                Ok(response) => report.check(
                    &format!("embeddings with {embed_model}"),
                    response.vectors.len() == 2 && response.vectors.iter().all(|v| !v.is_empty()),
                    &format!(
                        "{} vectors of {} dimensions",
                        response.vectors.len(),
                        response.vectors.first().map_or(0, Vec::len)
                    ),
                ),
                Err(err) => report.check(
                    &format!("embeddings with {embed_model}"),
                    false,
                    &err.to_string(),
                ),
            }
        }
        None => println!("skip embeddings — no embedding model listed (pass --embed-model)"),
    }

    println!("\n{} failed", report.failures);
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
