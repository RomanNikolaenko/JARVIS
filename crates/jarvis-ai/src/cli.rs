//! Cloud models used through a vendor CLI that the user installed and signed
//! in with their subscription: the OpenAI Codex CLI (ChatGPT) and Claude Code.
//!
//! JARVIS never handles those credentials; the CLI keeps them in its own
//! store. Every request starts the CLI once, in a new empty temporary folder,
//! and passes the conversation through stdin. The CLI's own tools, plugins,
//! MCP servers and hooks are switched off with its documented flags, so the
//! model answers with text only and JARVIS tools keep going through the
//! security policy. Those switches are requests to a third-party program,
//! not a sandbox JARVIS enforces. Dropping the stream kills the process tree.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::{
    AiError, AiProvider, ChatChunk, ChatMessage, ChatRequest, ChatRole, ChatStream, FinishReason,
    ModelCapabilities, ModelInfo, ProviderDescriptor, TokenUsage,
};

/// Model ID that leaves the choice to the CLI (the account's default).
pub const CLI_DEFAULT_MODEL: &str = "default";

/// Environment variables of JARVIS that a vendor CLI must not inherit.
const PRIVATE_ENV: &[&str] = &["LLAMA_API_KEY", "LLAMA_ARG_API_KEY_FILE"];
/// Longest stdout line accepted from a CLI (one JSON event).
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
/// How much of the CLI's stderr is kept for error messages.
const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// Longest error text shown to the user.
const MAX_ERROR_CHARS: usize = 1_000;
/// Time a CLI gets to report why it stopped after closing stdout.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// Time allowed for `login status` / `auth status`.
const STATUS_TIMEOUT: Duration = Duration::from_secs(6);

/// Replaces Claude Code's coding-agent prompt. JARVIS's own instructions
/// travel with the conversation (see [`transcript`]).
const CLAUDE_SYSTEM_PROMPT: &str = "You are the language model behind JARVIS, a desktop assistant. The user message holds JARVIS's instructions and the conversation so far. Follow those instructions and reply to the last user message. You have no tools.";

/// Codex features that give the model tools or start background services.
/// Unknown names only produce a warning event, so the list is safe across
/// CLI versions.
const CODEX_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "apps",
    "plugins",
    "hooks",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "image_generation",
    "multi_agent",
    "multi_agent_v2",
    "goals",
    "memories",
    "view_image",
    "remote_plugin",
    "skill_search",
    "tool_suggest",
    "sleep_tool",
    "code_mode_host",
    "daemon_auto_start",
];

/// Which vendor CLI a provider drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    /// OpenAI Codex CLI (`codex`), signed in with ChatGPT.
    Codex,
    /// Claude Code (`claude`), signed in with a Claude account.
    ClaudeCode,
}

impl CliKind {
    fn program_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }

    fn product(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::ClaudeCode => "Claude Code",
        }
    }

    fn login_command(self) -> &'static str {
        match self {
            Self::Codex => "codex login",
            Self::ClaudeCode => "claude auth login",
        }
    }

    fn status_args(self) -> [&'static str; 2] {
        match self {
            Self::Codex => ["login", "status"],
            Self::ClaudeCode => ["auth", "status"],
        }
    }

    /// Install folders used by the vendors' own installers, checked after
    /// `PATH`.
    fn known_locations(self) -> Vec<PathBuf> {
        let exe = std::env::consts::EXE_SUFFIX;
        match self {
            // The CLI bundled with the Codex desktop app.
            Self::Codex => codex_home()
                .map(|home| {
                    home.join("plugins")
                        .join(".plugin-appserver")
                        .join(format!("codex{exe}"))
                })
                .into_iter()
                .collect(),
            // The native installer of Claude Code.
            Self::ClaudeCode => home_dir()
                .map(|home| home.join(".local").join("bin").join(format!("claude{exe}")))
                .into_iter()
                .collect(),
        }
    }
}

/// A chat-only provider that runs a vendor CLI for every request.
#[derive(Debug)]
pub struct CliProvider {
    descriptor: ProviderDescriptor,
    kind: CliKind,
    program: Option<PathBuf>,
    codex_home: Option<PathBuf>,
}

impl CliProvider {
    pub fn new(descriptor: ProviderDescriptor, kind: CliKind) -> Self {
        Self {
            descriptor,
            kind,
            program: None,
            codex_home: None,
        }
    }

    /// Runs this program instead of looking for the CLI in `PATH` and the
    /// vendor's install folders.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// Reads the Codex model list from this folder instead of `CODEX_HOME`.
    pub fn with_codex_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.codex_home = Some(home.into());
        self
    }

    fn program(&self) -> Result<PathBuf, AiError> {
        match &self.program {
            Some(program) if program.is_file() => Ok(program.clone()),
            Some(program) => Err(AiError::Unavailable(format!(
                "{} was not found at {}",
                self.kind.product(),
                program.display()
            ))),
            None => find_in_path(self.kind.program_name())
                .or_else(|| {
                    self.kind
                        .known_locations()
                        .into_iter()
                        .find(|p| p.is_file())
                })
                .ok_or_else(|| {
                    AiError::Unavailable(format!(
                        "{} is not installed. Install it from its vendor, then sign in with `{}`",
                        self.kind.product(),
                        self.kind.login_command()
                    ))
                }),
        }
    }

    /// Fails unless the CLI reports a signed-in account. The CLI's output is
    /// not read: it may print account details.
    async fn check_signed_in(&self, program: &Path) -> Result<(), AiError> {
        let mut command = std::process::Command::new(program);
        command
            .args(self.kind.status_args())
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        scrub_env(&mut command);
        let mut child = jarvis_platform::spawn_owned(command).map_err(|err| {
            AiError::Unavailable(format!("cannot start {}: {err}", self.kind.product()))
        })?;
        match tokio::time::timeout(STATUS_TIMEOUT, child.wait_root()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(_)) => Err(AiError::Unavailable(format!(
                "{} is not signed in. Run `{}` in a terminal",
                self.kind.product(),
                self.kind.login_command()
            ))),
            Ok(Err(err)) => Err(AiError::Unavailable(format!(
                "{} failed: {err}",
                self.kind.product()
            ))),
            Err(_) => Err(AiError::Unavailable(format!(
                "{} did not report its sign-in status in time",
                self.kind.product()
            ))),
        }
    }

    fn models(&self, codex_cache: Option<&[u8]>) -> Vec<ModelInfo> {
        let model = |id: &str, name: &str, context: Option<u32>, vision: bool| ModelInfo {
            id: id.to_owned(),
            display_name: name.to_owned(),
            provider_id: self.descriptor.id.clone(),
            context_window: context,
            capabilities: ModelCapabilities {
                chat: true,
                streaming: true,
                vision,
                ..ModelCapabilities::default()
            },
        };
        match self.kind {
            CliKind::Codex => {
                let mut models = vec![model(
                    CLI_DEFAULT_MODEL,
                    "Default (chosen by Codex)",
                    None,
                    false,
                )];
                models.extend(
                    codex_cache
                        .map(codex_cached_models)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|m| {
                            model(
                                &m.slug,
                                m.display_name.as_deref().unwrap_or(&m.slug),
                                m.context_window.and_then(|n| u32::try_from(n).ok()),
                                m.input_modalities.iter().any(|kind| kind == "image"),
                            )
                        }),
                );
                models
            }
            // Aliases documented for `claude --model`; the account decides
            // which model each one means.
            CliKind::ClaudeCode => vec![
                model(
                    CLI_DEFAULT_MODEL,
                    "Default (chosen by Claude Code)",
                    None,
                    false,
                ),
                model("opus", "Opus (latest)", None, false),
                model("sonnet", "Sonnet (latest)", None, false),
                model("haiku", "Haiku (latest)", None, false),
                model("fable", "Fable (latest)", None, false),
            ],
        }
    }

    fn codex_home(&self) -> Option<PathBuf> {
        self.codex_home.clone().or_else(codex_home)
    }
}

#[async_trait]
impl AiProvider for CliProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    /// Only checks that the CLI is installed; starting it on every health
    /// probe would be too slow.
    async fn health_check(&self) -> Result<(), AiError> {
        self.program().map(drop)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        let program = self.program()?;
        self.check_signed_in(&program).await?;
        let cache = match (self.kind, self.codex_home()) {
            (CliKind::Codex, Some(home)) => {
                tokio::fs::read(home.join("models_cache.json")).await.ok()
            }
            _ => None,
        };
        Ok(self.models(cache.as_deref()))
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<ChatStream, AiError> {
        if request.messages.is_empty() {
            return Err(AiError::InvalidRequest("no messages".into()));
        }
        if request.model != CLI_DEFAULT_MODEL && !is_safe_model_id(&request.model) {
            return Err(AiError::ModelNotFound {
                model: request.model,
            });
        }
        let program = self.program()?;
        let dir = tempfile::Builder::new()
            .prefix("jarvis-cli-")
            .tempdir()
            .map_err(|err| AiError::Unavailable(format!("cannot create a work folder: {err}")))?;

        let (args, images) = match self.kind {
            CliKind::Codex => {
                let images = write_images(dir.path(), &request.messages)?;
                (codex_args(dir.path(), &request.model, &images), images)
            }
            CliKind::ClaudeCode => {
                if request.messages.iter().any(|m| !m.images.is_empty()) {
                    return Err(AiError::InvalidRequest(
                        "images are not supported through Claude Code".into(),
                    ));
                }
                (claude_args(&request.model), Vec::new())
            }
        };
        let prompt = transcript(&request.messages, &images, self.kind);

        let mut command = std::process::Command::new(&program);
        command
            .args(&args)
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        scrub_env(&mut command);
        let mut process = jarvis_platform::spawn_owned(command).map_err(|err| {
            AiError::Unavailable(format!("cannot start {}: {err}", self.kind.product()))
        })?;
        tracing::debug!(program = %program.display(), "started a CLI chat request");

        let stdin = process.stdin().take();
        let stdout = process.stdout().take();
        let stderr = process.stderr().take();
        let (Some(mut stdin), Some(stdout), Some(stderr)) = (stdin, stdout, stderr) else {
            return Err(AiError::Unavailable("the CLI's pipes are missing".into()));
        };
        // CLIs read the whole prompt before they answer. A failed write means
        // the CLI stopped early; its output says why.
        if stdin.write_all(prompt.as_bytes()).await.is_ok() {
            let _ = stdin.shutdown().await;
        }
        drop(stdin);

        let run = Run {
            _process: process,
            _dir: dir,
            output: stream::select(lines(stdout), stderr_chunks(stderr)).boxed(),
            parser: match self.kind {
                CliKind::Codex => Parser::Codex(CodexParser::default()),
                CliKind::ClaudeCode => Parser::Claude(ClaudeParser::default()),
            },
            kind: self.kind,
            stderr: Vec::new(),
            pending: VecDeque::new(),
        };
        Ok(stream::unfold(Some(run), next_chunk).boxed())
    }
}

/// One running request. Dropping it kills the CLI and deletes the folder.
struct Run {
    _process: jarvis_platform::OwnedProcess,
    _dir: tempfile::TempDir,
    output: BoxStream<'static, Output>,
    parser: Parser,
    kind: CliKind,
    stderr: Vec<u8>,
    pending: VecDeque<Result<ChatChunk, AiError>>,
}

enum Output {
    Line(String),
    LineTooLong,
    StdoutClosed,
    Stderr(Vec<u8>),
}

async fn next_chunk(state: Option<Run>) -> Option<(Result<ChatChunk, AiError>, Option<Run>)> {
    let mut run = state?;
    loop {
        if let Some(item) = run.pending.pop_front() {
            let last = matches!(item, Ok(ChatChunk::Done { .. }) | Err(_));
            // The final item drops the run: the CLI has nothing more to say.
            return Some((item, (!last).then_some(run)));
        }
        match run.output.next().await {
            Some(Output::Line(line)) => run.parser.line(&line, run.kind, &mut run.pending),
            Some(Output::Stderr(bytes)) => keep_tail(&mut run.stderr, &bytes),
            Some(Output::LineTooLong) => run.pending.push_back(Err(AiError::Protocol(
                "the CLI printed an oversized line".into(),
            ))),
            Some(Output::StdoutClosed) | None => {
                // Give the CLI a moment to finish its error output.
                let drain = async {
                    while let Some(output) = run.output.next().await {
                        if let Output::Stderr(bytes) = output {
                            keep_tail(&mut run.stderr, &bytes);
                        }
                    }
                };
                let _ = tokio::time::timeout(EXIT_GRACE, drain).await;
                let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
                let message = run.parser.last_error().unwrap_or(stderr);
                run.pending.push_back(Err(failure(run.kind, &message)));
            }
        }
    }
}

enum Parser {
    Codex(CodexParser),
    Claude(ClaudeParser),
}

impl Parser {
    fn line(&mut self, line: &str, kind: CliKind, out: &mut VecDeque<Result<ChatChunk, AiError>>) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            tracing::debug!("ignored a non-JSON line from a CLI");
            return;
        };
        match self {
            Self::Codex(parser) => parser.event(&event, kind, out),
            Self::Claude(parser) => parser.event(&event, kind, out),
        }
    }

    fn last_error(&self) -> Option<String> {
        match self {
            Self::Codex(parser) => parser.last_error.clone(),
            Self::Claude(_) => None,
        }
    }
}

/// `codex exec --json` events: whole agent messages, then `turn.completed`
/// or `turn.failed`.
#[derive(Default)]
struct CodexParser {
    wrote_text: bool,
    last_error: Option<String>,
}

impl CodexParser {
    fn event(
        &mut self,
        event: &Value,
        kind: CliKind,
        out: &mut VecDeque<Result<ChatChunk, AiError>>,
    ) {
        match event["type"].as_str() {
            Some("item.completed") => {
                let item = &event["item"];
                match item["type"].as_str() {
                    Some("agent_message") => {
                        if let Some(text) = item["text"].as_str().filter(|t| !t.is_empty()) {
                            let separator = if self.wrote_text { "\n\n" } else { "" };
                            self.wrote_text = true;
                            out.push_back(Ok(ChatChunk::Delta(format!("{separator}{text}"))));
                        }
                    }
                    // Warnings such as an unknown setting; the turn goes on.
                    Some("error") => tracing::debug!(
                        message = item["message"].as_str().unwrap_or_default(),
                        "Codex warning"
                    ),
                    _ => {}
                }
            }
            Some("error") => {
                self.last_error = event["message"].as_str().map(str::to_owned);
            }
            Some("turn.failed") => {
                let message = event["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.last_error.clone())
                    .unwrap_or_default();
                out.push_back(Err(failure(kind, &message)));
            }
            Some("turn.completed") => {
                let usage = &event["usage"];
                out.push_back(Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: Some(TokenUsage {
                        prompt_tokens: token_count(&usage["input_tokens"]),
                        completion_tokens: token_count(&usage["output_tokens"]),
                    }),
                }));
            }
            _ => {}
        }
    }
}

/// `claude -p --output-format stream-json` events: partial text deltas,
/// then one `result`.
#[derive(Default)]
struct ClaudeParser {
    wrote_text: bool,
    finish: Option<FinishReason>,
    refusal: bool,
}

impl ClaudeParser {
    fn event(
        &mut self,
        event: &Value,
        kind: CliKind,
        out: &mut VecDeque<Result<ChatChunk, AiError>>,
    ) {
        match event["type"].as_str() {
            Some("stream_event") => {
                let inner = &event["event"];
                match inner["type"].as_str() {
                    Some("content_block_delta") if inner["delta"]["type"] == "text_delta" => {
                        if let Some(text) =
                            inner["delta"]["text"].as_str().filter(|t| !t.is_empty())
                        {
                            self.wrote_text = true;
                            out.push_back(Ok(ChatChunk::Delta(text.to_owned())));
                        }
                    }
                    Some("message_delta") => match inner["delta"]["stop_reason"].as_str() {
                        Some("max_tokens") => self.finish = Some(FinishReason::Length),
                        Some("refusal") => self.refusal = true,
                        _ => {}
                    },
                    _ => {}
                }
            }
            Some("result") => {
                let text = event["result"].as_str().unwrap_or_default();
                if event["is_error"].as_bool().unwrap_or(false) {
                    let message = if text.is_empty() {
                        event["subtype"].as_str().unwrap_or_default()
                    } else {
                        text
                    };
                    out.push_back(Err(failure(kind, message)));
                    return;
                }
                if self.refusal {
                    out.push_back(Err(AiError::Refused(clip(text))));
                    return;
                }
                // Without partial events the whole answer arrives here.
                if !self.wrote_text && !text.is_empty() {
                    out.push_back(Ok(ChatChunk::Delta(text.to_owned())));
                }
                let usage = &event["usage"];
                let prompt = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .filter_map(|key| token_count(&usage[*key]))
                .reduce(u32::saturating_add);
                out.push_back(Ok(ChatChunk::Done {
                    finish_reason: self.finish.unwrap_or(FinishReason::Stop),
                    usage: Some(TokenUsage {
                        prompt_tokens: prompt,
                        completion_tokens: token_count(&usage["output_tokens"]),
                    }),
                }));
            }
            _ => {}
        }
    }
}

fn codex_args(dir: &Path, model: &str, images: &[PathBuf]) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--ignore-user-config",
        "--ignore-rules",
        "--color",
        "never",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push("-C".into());
    args.push(dir.display().to_string());
    for feature in CODEX_DISABLED_FEATURES {
        args.push("-c".into());
        args.push(format!("features.{feature}=false"));
    }
    args.push("-c".into());
    args.push("web_search=disabled".into());
    if model != CLI_DEFAULT_MODEL {
        args.push("-m".into());
        args.push(model.to_owned());
    }
    for image in images {
        // `--image=` takes exactly one file, so `-` below stays the prompt.
        args.push(format!("--image={}", image.display()));
    }
    args.push("-".into());
    args
}

fn claude_args(model: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--safe-mode",
        "--tools",
        "",
        "--disallowedTools",
        "mcp__*",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--max-turns",
        "1",
        "--system-prompt",
        CLAUDE_SYSTEM_PROMPT,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if model != CLI_DEFAULT_MODEL {
        args.push("--model".into());
        args.push(model.to_owned());
    }
    args
}

/// The conversation as one prompt. A CLI takes a single message per run, so
/// the history and JARVIS's instructions travel inside it.
fn transcript(messages: &[ChatMessage], images: &[PathBuf], kind: CliKind) -> String {
    let mut text = String::new();
    if kind == CliKind::Codex {
        text.push_str(
            "You are the language model behind JARVIS, a desktop assistant. Below are JARVIS's instructions and the conversation so far. Follow the instructions and reply to the last user message with the answer only; do not run commands or edit files.\n\n",
        );
    }
    let instructions: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == ChatRole::System)
        .map(|m| m.content.trim())
        .filter(|c| !c.is_empty())
        .collect();
    if !instructions.is_empty() {
        let _ = write!(
            text,
            "<instructions>\n{}\n</instructions>\n\n",
            instructions.join("\n\n")
        );
    }
    let mut image_names = images.iter().filter_map(|p| p.file_name());
    for message in messages.iter().filter(|m| m.role != ChatRole::System) {
        let role = match message.role {
            ChatRole::Assistant => "assistant",
            _ => "user",
        };
        let _ = write!(text, "<message role=\"{role}\">\n{}\n", message.content);
        for _ in &message.images {
            if let Some(name) = image_names.next() {
                let _ = writeln!(text, "[attached image: {}]", name.to_string_lossy());
            }
        }
        text.push_str("</message>\n");
    }
    text
}

/// Saves the conversation's images for `--image`. They live in the request's
/// private temporary folder and go away with it.
fn write_images(dir: &Path, messages: &[ChatMessage]) -> Result<Vec<PathBuf>, AiError> {
    let mut paths = Vec::new();
    for image in messages.iter().flat_map(|m| &m.images) {
        let extension = match image.media_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            other => {
                return Err(AiError::InvalidRequest(format!(
                    "unsupported image type {other}"
                )));
            }
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.base64)
            .map_err(|err| AiError::InvalidRequest(format!("invalid image data: {err}")))?;
        let path = dir.join(format!("image-{}.{extension}", paths.len() + 1));
        std::fs::write(&path, bytes)
            .map_err(|err| AiError::Unavailable(format!("cannot save an image: {err}")))?;
        paths.push(path);
    }
    Ok(paths)
}

/// Turns a CLI's error text into an error the user can act on.
fn failure(kind: CliKind, message: &str) -> AiError {
    let message = clip(message.trim());
    let lower = message.to_lowercase();
    if message.is_empty() {
        AiError::Unavailable(format!("{} stopped without an answer", kind.product()))
    } else if [
        "usage limit",
        "hit your limit",
        "limit reached",
        "rate limit",
        "quota",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        AiError::LimitReached(message)
    } else if [
        "not logged in",
        "log in",
        "login",
        "sign in",
        "unauthorized",
        "authenticat",
        "invalid api key",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        AiError::Unavailable(format!(
            "{message} (sign in with `{}`)",
            kind.login_command()
        ))
    } else {
        AiError::Unavailable(format!("{}: {message}", kind.product()))
    }
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_ERROR_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn token_count(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn keep_tail(buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(bytes);
    if buffer.len() > STDERR_TAIL_BYTES {
        let excess = buffer.len() - STDERR_TAIL_BYTES;
        buffer.drain(..excess);
    }
}

/// Model IDs become a command-line argument: no option-like or odd values.
fn is_safe_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '[' | ']'))
}

fn lines(stdout: impl AsyncRead + Unpin + Send + 'static) -> BoxStream<'static, Output> {
    stream::unfold(Some(BufReader::new(stdout)), |reader| async move {
        let mut reader = reader?;
        let mut buffer = Vec::new();
        match (&mut reader)
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut buffer)
            .await
        {
            Ok(0) | Err(_) => Some((Output::StdoutClosed, None)),
            Ok(_) if !buffer.ends_with(b"\n") && buffer.len() as u64 >= MAX_LINE_BYTES => {
                Some((Output::LineTooLong, None))
            }
            Ok(_) => Some((
                Output::Line(String::from_utf8_lossy(&buffer).into_owned()),
                Some(reader),
            )),
        }
    })
    .boxed()
}

fn stderr_chunks(stderr: impl AsyncRead + Unpin + Send + 'static) -> BoxStream<'static, Output> {
    stream::unfold(stderr, |mut stderr| async move {
        let mut buffer = vec![0; 4096];
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                buffer.truncate(n);
                Some((Output::Stderr(buffer), stderr))
            }
        }
    })
    .boxed()
}

fn scrub_env(command: &mut std::process::Command) {
    for name in PRIVATE_ENV {
        command.env_remove(name);
    }
}

/// Finds a program in `PATH`, skipping relative entries. On Windows `.exe`
/// and `.cmd` files are considered.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| {
            names
                .iter()
                .map(|n| dir.join(n))
                .find(|candidate| candidate.is_file())
        })
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home_dir().map(|home| home.join(".codex")))
}

#[derive(Deserialize)]
struct CodexModelsCache {
    #[serde(default)]
    models: Vec<CodexCachedModel>,
}

#[derive(Deserialize)]
struct CodexCachedModel {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    input_modalities: Vec<String>,
}

/// Models listed for the signed-in account in the Codex CLI's own cache.
/// The cache format is not a public API, so anything unexpected yields
/// no models rather than an error.
fn codex_cached_models(bytes: &[u8]) -> Vec<CodexCachedModel> {
    serde_json::from_slice::<CodexModelsCache>(bytes)
        .map(|cache| {
            cache
                .models
                .into_iter()
                .filter(|m| m.visibility.as_deref() == Some("list"))
                .filter(|m| m.slug != CLI_DEFAULT_MODEL && is_safe_model_id(&m.slug))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImageData;

    fn events(kind: CliKind, lines: &[&str]) -> Vec<Result<ChatChunk, AiError>> {
        let mut parser = match kind {
            CliKind::Codex => Parser::Codex(CodexParser::default()),
            CliKind::ClaudeCode => Parser::Claude(ClaudeParser::default()),
        };
        let mut out = VecDeque::new();
        for line in lines {
            parser.line(line, kind, &mut out);
        }
        out.into_iter().collect()
    }

    #[test]
    fn reads_codex_events() {
        let out = events(
            CliKind::Codex,
            &[
                r#"{"type":"thread.started","thread_id":"t"}"#,
                r#"{"type":"item.completed","item":{"id":"0","type":"error","message":"unknown setting"}}"#,
                r#"{"type":"item.completed","item":{"id":"1","type":"reasoning","text":"thinking"}}"#,
                r#"{"type":"item.completed","item":{"id":"2","type":"agent_message","text":"Привіт"}}"#,
                r#"{"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":0,"output_tokens":3}}"#,
            ],
        );
        assert_eq!(
            out,
            vec![
                Ok(ChatChunk::Delta("Привіт".into())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Stop,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(12),
                        completion_tokens: Some(3)
                    }),
                }),
            ]
        );
    }

    #[test]
    fn reports_codex_limits_and_sign_in_problems() {
        let out = events(
            CliKind::Codex,
            &[
                r#"{"type":"error","message":"You've hit your usage limit. Try again at 4:26 PM."}"#,
                r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Try again at 4:26 PM."}}"#,
            ],
        );
        assert!(matches!(out.as_slice(), [Err(AiError::LimitReached(m))] if m.contains("4:26 PM")));
        match failure(CliKind::Codex, "Not logged in") {
            AiError::Unavailable(message) => assert!(message.contains("codex login")),
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            failure(CliKind::ClaudeCode, "  "),
            AiError::Unavailable(_)
        ));
    }

    #[test]
    fn reads_claude_code_events() {
        let out = events(
            CliKind::ClaudeCode,
            &[
                r#"{"type":"system","subtype":"init","model":"claude-opus-5-5"}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Го"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"тово"}}}"#,
                r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Готово"}]}}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Готово","usage":{"input_tokens":5,"cache_read_input_tokens":100,"output_tokens":2}}"#,
            ],
        );
        assert_eq!(
            out,
            vec![
                Ok(ChatChunk::Delta("Го".into())),
                Ok(ChatChunk::Delta("тово".into())),
                Ok(ChatChunk::Done {
                    finish_reason: FinishReason::Length,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(105),
                        completion_tokens: Some(2)
                    }),
                }),
            ]
        );
        // Without partial events the result carries the answer.
        let out = events(
            CliKind::ClaudeCode,
            &[r#"{"type":"result","is_error":false,"result":"Так","usage":{}}"#],
        );
        assert_eq!(out[0], Ok(ChatChunk::Delta("Так".into())));
    }

    #[test]
    fn reports_claude_code_errors_and_refusals() {
        let out = events(
            CliKind::ClaudeCode,
            &[
                r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#,
            ],
        );
        assert!(
            matches!(out.as_slice(), [Err(AiError::Unavailable(m))] if m.contains("claude auth login"))
        );
        let out = events(
            CliKind::ClaudeCode,
            &[
                r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"refusal"}}}"#,
                r#"{"type":"result","is_error":false,"result":"declined"}"#,
            ],
        );
        assert!(matches!(out.as_slice(), [Err(AiError::Refused(_))]));
    }

    #[test]
    fn builds_a_transcript_with_instructions_history_and_images() {
        let image = ImageData {
            media_type: "image/png".into(),
            base64: "iVBORw0KGgo=".into(),
        };
        let messages = vec![
            ChatMessage::new(ChatRole::System, "Be brief."),
            ChatMessage::user("Hi"),
            ChatMessage::new(ChatRole::Assistant, "Hello!"),
            ChatMessage::user("What is this?").with_images(vec![image]),
        ];
        let dir = tempfile::tempdir().expect("dir");
        let images = write_images(dir.path(), &messages).expect("images");
        assert_eq!(images.len(), 1);
        assert_eq!(
            std::fs::read(&images[0]).expect("image"),
            b"\x89PNG\r\n\x1a\n"
        );
        let text = transcript(&messages, &images, CliKind::ClaudeCode);
        assert_eq!(
            text,
            "<instructions>\nBe brief.\n</instructions>\n\n<message role=\"user\">\nHi\n</message>\n<message role=\"assistant\">\nHello!\n</message>\n<message role=\"user\">\nWhat is this?\n[attached image: image-1.png]\n</message>\n"
        );
        assert!(
            transcript(&messages, &images, CliKind::Codex)
                .starts_with("You are the language model behind JARVIS")
        );
    }

    #[test]
    fn disables_the_clis_own_tools() {
        let args = codex_args(Path::new("work"), "gpt-x", &[PathBuf::from("image-1.png")]);
        for expected in [
            "--sandbox",
            "read-only",
            "--ignore-user-config",
            "--ephemeral",
            "features.shell_tool=false",
            "features.apps=false",
            "web_search=disabled",
            "--image=image-1.png",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected} missing");
        }
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert!(!codex_args(Path::new("w"), CLI_DEFAULT_MODEL, &[]).contains(&"-m".to_owned()));

        let args = claude_args("sonnet");
        let tools = args.iter().position(|a| a == "--tools").expect("--tools");
        assert_eq!(args[tools + 1], "");
        for expected in [
            "--safe-mode",
            "--no-session-persistence",
            "mcp__*",
            "sonnet",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected} missing");
        }
        assert!(!args.iter().any(|a| a == "--bare"));
    }

    #[test]
    fn model_ids_cannot_become_options() {
        assert!(is_safe_model_id("gpt-6-astra"));
        assert!(is_safe_model_id("sonnet[1m]"));
        assert!(!is_safe_model_id("--dangerously-skip-permissions"));
        assert!(!is_safe_model_id("a b"));
        assert!(!is_safe_model_id(""));
    }

    #[test]
    fn lists_models_from_the_codex_cache() {
        let cache = br#"{"models":[
            {"slug":"gpt-a","display_name":"GPT A","visibility":"list","context_window":272000,"input_modalities":["text","image"]},
            {"slug":"hidden","visibility":"hide"},
            {"slug":"-x","visibility":"list"}
        ]}"#;
        let models = codex_cached_models(cache);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-a");
        assert!(codex_cached_models(b"not json").is_empty());
    }

    #[test]
    fn keeps_only_the_end_of_stderr() {
        let mut buffer = Vec::new();
        keep_tail(&mut buffer, &vec![b'a'; STDERR_TAIL_BYTES]);
        keep_tail(&mut buffer, b"end");
        assert_eq!(buffer.len(), STDERR_TAIL_BYTES);
        assert!(buffer.ends_with(b"end"));
    }
}
