//! A minimal MCP client over a byte stream (a server's stdio).
//!
//! Supports both protocol generations: the current revision (2026-07-28),
//! where every request carries the version and client capabilities in
//! `_meta` and the server describes itself through `server/discover`, and
//! the older revisions with the `initialize` handshake. The client declares
//! no capabilities of its own (no sampling, roots, elicitation, logging).
//!
//! The server is untrusted: messages are size-limited, every request has a
//! deadline, and nothing the server sends can make the client loop.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::error::{McpError, RpcError};
use crate::types::ToolHints;

/// Protocol revisions without a handshake, newest first.
pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
/// Revisions with the `initialize` handshake, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Largest message accepted from a server.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// Most tools taken from one server.
pub const MAX_TOOLS: usize = 500;
/// Longest tool name accepted (the specification's limit).
pub const MAX_TOOL_NAME: usize = 128;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_TOOL_PAGES: usize = 20;
/// Retries of a call the server answered with `input_required` and only a
/// `requestState`.
const MAX_STATE_RETRIES: usize = 3;
/// How long the client waits for an answer to `server/discover` before it
/// also tries the legacy handshake (at most a third of the whole limit).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
const MISSING_CLIENT_CAPABILITY: i64 = -32021;
const METHOD_NOT_FOUND: i64 = -32601;

/// Protocol generation of a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// 2026-07-28 and later: no handshake, `_meta` on every request.
    Modern,
    /// 2025-11-25 and earlier: `initialize` first.
    Legacy,
}

/// How JARVIS introduces itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// What a server says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerDescription {
    pub name: String,
    pub version: String,
    pub title: Option<String>,
    pub protocol_version: String,
    pub era: Era,
    /// Instructions for the model; untrusted text.
    pub instructions: Option<String>,
    /// The server announced tools.
    pub has_tools: bool,
}

/// A tool as the server describes it (untrusted text).
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub hints: ToolHints,
}

/// The result of a tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct CallResult {
    pub content: Vec<Content>,
    pub structured: Option<Value>,
    /// The tool reports that it failed.
    pub is_error: bool,
}

/// One block of a tool result.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    Image { mime_type: String, bytes: usize },
    Audio { mime_type: String, bytes: usize },
    ResourceLink { uri: String, name: String },
    Resource { uri: String, text: Option<String> },
    Other(String),
}

impl CallResult {
    /// The result as text for a model: text blocks as they are, other
    /// blocks as short notes, structured content when there is no text.
    pub fn to_text(&self) -> String {
        let mut parts: Vec<String> = self
            .content
            .iter()
            .map(|block| match block {
                Content::Text(text) => text.clone(),
                Content::Image { mime_type, bytes } => {
                    format!(
                        "[image {mime_type}, {} KB, not shown]",
                        bytes.div_ceil(1024)
                    )
                }
                Content::Audio { mime_type, bytes } => {
                    format!(
                        "[audio {mime_type}, {} KB, not shown]",
                        bytes.div_ceil(1024)
                    )
                }
                Content::ResourceLink { uri, name } => format!("[link {name}: {uri}]"),
                Content::Resource {
                    uri,
                    text: Some(text),
                } => format!("[resource {uri}]\n{text}"),
                Content::Resource { uri, text: None } => {
                    format!("[binary resource {uri}, not shown]")
                }
                Content::Other(kind) => format!("[{kind} content, not shown]"),
            })
            .collect();
        let has_text = self.content.iter().any(|b| matches!(b, Content::Text(_)));
        if !has_text && let Some(structured) = &self.structured {
            parts.push(structured.to_string());
        }
        parts.join("\n")
    }
}

type Reply = Result<Value, RpcError>;

/// State shared with the reader and writer tasks.
#[derive(Debug)]
struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    /// Negotiated generation and version.
    protocol: Mutex<Option<(Era, String)>>,
    closed: CancellationToken,
    close_reason: Mutex<Option<String>>,
    tools_changed: watch::Sender<u64>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn close(&self, reason: impl Into<String>) {
        {
            let mut current = lock(&self.close_reason);
            if current.is_none() {
                *current = Some(reason.into());
            }
        }
        self.closed.cancel();
        // Dropping the senders wakes every waiting request.
        lock(&self.pending).clear();
    }

    fn era(&self) -> Option<Era> {
        lock(&self.protocol).as_ref().map(|(era, _)| *era)
    }

    fn dispatch(&self, message: Value, outgoing: &mpsc::UnboundedSender<String>) {
        let Value::Object(map) = message else {
            return;
        };
        let method = map.get("method").and_then(Value::as_str);
        match (method, map.get("id")) {
            (Some(method), Some(id)) => self.answer_request(method, id, outgoing),
            (Some(method), None) => self.notification(method),
            (None, Some(id)) => {
                let Some(id) = id.as_u64() else {
                    return;
                };
                let Some(sender) = lock(&self.pending).remove(&id) else {
                    // An answer after a timeout or cancellation.
                    return;
                };
                let reply = match map.get("error") {
                    Some(error) => Err(parse_rpc_error(error)),
                    None => Ok(map.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(reply);
            }
            (None, None) => {}
        }
    }

    /// Legacy servers may ping; JARVIS offers no other client features.
    /// Modern servers must not send requests, and clients must not answer.
    fn answer_request(&self, method: &str, id: &Value, outgoing: &mpsc::UnboundedSender<String>) {
        if self.era() == Some(Era::Modern) {
            tracing::debug!(target: "jarvis_plugins::mcp", "ignored a request from a modern server: {method}");
            return;
        }
        let answer = if method == "ping" {
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": METHOD_NOT_FOUND, "message": format!("JARVIS does not support {method}")},
            })
        };
        let _ = outgoing.send(answer.to_string());
    }

    fn notification(&self, method: &str) {
        match method {
            "notifications/tools/list_changed" => {
                self.tools_changed.send_modify(|count| *count += 1);
            }
            "notifications/message" => {
                tracing::debug!(target: "jarvis_plugins::mcp", "log message from a server");
            }
            _ => {}
        }
    }
}

/// A deadline with the limit it was made from (for error messages).
#[derive(Debug, Clone, Copy)]
struct Deadline {
    at: Instant,
    total: Duration,
}

impl Deadline {
    fn after(total: Duration) -> Self {
        Self {
            at: Instant::now() + total,
            total,
        }
    }

    fn timed_out(&self) -> McpError {
        McpError::TimedOut(self.total.as_secs().max(1))
    }
}

/// A connection to one MCP server.
#[derive(Debug)]
pub struct McpClient {
    shared: Arc<Shared>,
    outgoing: mpsc::UnboundedSender<String>,
    next_id: AtomicU64,
    info: ClientInfo,
    /// Closes the server's input.
    shutdown: CancellationToken,
}

impl McpClient {
    /// Starts reading and writing; nothing is sent until [`Self::negotiate`].
    /// Must be called inside a Tokio runtime.
    pub fn start<R, W>(reader: R, writer: W, info: ClientInfo) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let shared = Arc::new(Shared {
            pending: Mutex::default(),
            protocol: Mutex::default(),
            closed: CancellationToken::new(),
            close_reason: Mutex::default(),
            tools_changed: watch::channel(0).0,
        });
        let (outgoing, incoming) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        tokio::spawn(write_loop(
            writer,
            incoming,
            shutdown.clone(),
            Arc::clone(&shared),
        ));
        tokio::spawn(read_loop(reader, Arc::clone(&shared), outgoing.clone()));
        Arc::new(Self {
            shared,
            outgoing,
            next_id: AtomicU64::new(1),
            info,
            shutdown,
        })
    }

    /// [`Self::start`] and [`Self::negotiate`].
    pub async fn connect<R, W>(
        reader: R,
        writer: W,
        info: ClientInfo,
        timeout: Duration,
    ) -> Result<(Arc<Self>, ServerDescription), McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let client = Self::start(reader, writer, info);
        match client.negotiate(timeout).await {
            Ok(description) => Ok((client, description)),
            Err(err) => {
                client.close();
                Err(err)
            }
        }
    }

    /// Ends the connection: pending requests fail and the server's input is
    /// closed, which asks a stdio server to exit.
    pub fn close(&self) {
        self.shared.close("closed by JARVIS");
        self.shutdown.cancel();
    }

    /// Ends the connection because the server process exited.
    pub(crate) fn end(&self, reason: String) {
        self.shared.close(reason);
        self.shutdown.cancel();
    }

    /// Cancelled when the connection ends.
    pub fn closed(&self) -> CancellationToken {
        self.shared.closed.clone()
    }

    pub fn close_reason(&self) -> Option<String> {
        lock(&self.shared.close_reason).clone()
    }

    /// Counts `notifications/tools/list_changed` from the server.
    pub fn tools_changed(&self) -> watch::Receiver<u64> {
        self.shared.tools_changed.subscribe()
    }

    fn closed_error(&self) -> McpError {
        McpError::Closed(
            self.close_reason()
                .unwrap_or_else(|| "the server stopped answering".to_owned()),
        )
    }

    fn send(&self, message: &Value) -> Result<(), McpError> {
        // `to_string` escapes newlines inside strings, as stdio requires.
        self.outgoing
            .send(message.to_string())
            .map_err(|_| self.closed_error())
    }

    fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(u64, oneshot::Receiver<Reply>), McpError> {
        if self.shared.closed.is_cancelled() {
            return Err(self.closed_error());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.shared.pending).insert(id, sender);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok((id, receiver))
    }

    /// Stops waiting for a request; the server is told when the protocol
    /// is set up (late answers are ignored either way).
    fn forget(&self, id: u64, reason: &str) {
        lock(&self.shared.pending).remove(&id);
        if self.shared.era().is_some() {
            let _ = self.send(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": id, "reason": reason},
            }));
        }
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        deadline: Deadline,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, McpError> {
        let (id, receiver) = self.send_request(method, params)?;
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        tokio::select! {
            reply = receiver => match reply {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(error)) => Err(McpError::Rpc(error)),
                Err(_) => Err(self.closed_error()),
            },
            () = tokio::time::sleep_until(deadline.at.into()) => {
                self.forget(id, "timed out");
                Err(deadline.timed_out())
            }
            () = cancel.cancelled() => {
                self.forget(id, "cancelled by the user");
                Err(McpError::Cancelled)
            }
        }
    }

    fn meta(&self, version: &str) -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": self.info.name, "version": self.info.version},
        })
    }

    /// Request parameters for the negotiated protocol.
    fn params(&self, mut params: Map<String, Value>) -> Value {
        if let Some((Era::Modern, version)) = lock(&self.shared.protocol).clone() {
            params.insert("_meta".to_owned(), self.meta(&version));
        }
        Value::Object(params)
    }

    fn discover_params(&self, version: &str) -> Value {
        json!({"_meta": self.meta(version)})
    }

    fn initialize_params(&self, version: &str) -> Value {
        json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": self.info.name, "version": self.info.version},
        })
    }

    /// Finds the server's protocol generation and version.
    ///
    /// Probes with `server/discover`; a result or an "unsupported protocol
    /// version" error means a modern server, any other error a legacy one.
    /// Without an answer after a few seconds the legacy `initialize` is sent
    /// too (the server may be legacy and silent, or just slow to start), and
    /// the first success decides.
    pub async fn negotiate(&self, timeout: Duration) -> Result<ServerDescription, McpError> {
        let deadline = Deadline::after(timeout);
        let preferred = MODERN_VERSIONS[0];
        let (discover_id, mut discover) =
            self.send_request("server/discover", self.discover_params(preferred))?;
        let probe_end = Instant::now() + PROBE_TIMEOUT.min(timeout / 3);
        tokio::select! {
            reply = &mut discover => return self.after_discover(reply, deadline).await,
            () = tokio::time::sleep_until(probe_end.into()) => {}
        }

        let (initialize_id, initialize) =
            self.send_request("initialize", self.initialize_params(LEGACY_VERSIONS[0]))?;
        let mut discover = Some(discover);
        let mut initialize = Some(initialize);
        let mut last_error = None;
        while discover.is_some() || initialize.is_some() {
            tokio::select! {
                reply = next(&mut discover) => {
                    discover = None;
                    match reply {
                        Ok(Ok(result)) => {
                            lock(&self.shared.pending).remove(&initialize_id);
                            return self.modern(&result, preferred);
                        }
                        Ok(Err(error)) if error.code == UNSUPPORTED_PROTOCOL_VERSION => {
                            lock(&self.shared.pending).remove(&initialize_id);
                            return self.after_discover(Ok(Err(error)), deadline).await;
                        }
                        Ok(Err(error)) => last_error = Some(McpError::Rpc(error)),
                        Err(_) => return Err(self.closed_error()),
                    }
                }
                reply = next(&mut initialize) => {
                    initialize = None;
                    match reply {
                        Ok(Ok(result)) => {
                            lock(&self.shared.pending).remove(&discover_id);
                            return self.legacy(&result);
                        }
                        Ok(Err(error)) => last_error = Some(McpError::Rpc(error)),
                        Err(_) => return Err(self.closed_error()),
                    }
                }
                () = tokio::time::sleep_until(deadline.at.into()) => {
                    return Err(deadline.timed_out());
                }
            }
        }
        Err(last_error.unwrap_or_else(|| self.closed_error()))
    }

    async fn after_discover(
        &self,
        reply: Result<Reply, oneshot::error::RecvError>,
        deadline: Deadline,
    ) -> Result<ServerDescription, McpError> {
        match reply {
            Ok(Ok(result)) => self.modern(&result, MODERN_VERSIONS[0]),
            Ok(Err(error)) if error.code == UNSUPPORTED_PROTOCOL_VERSION => {
                let supported: Vec<String> = error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("supported"))
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(version) = pick(&supported, MODERN_VERSIONS) {
                    let result = self
                        .request(
                            "server/discover",
                            self.discover_params(version),
                            deadline,
                            None,
                        )
                        .await?;
                    self.modern(&result, version)
                } else if let Some(version) = pick(&supported, LEGACY_VERSIONS) {
                    self.initialize(version, deadline).await
                } else {
                    Err(unsupported_versions(&supported))
                }
            }
            // Legacy servers answer unknown requests before `initialize`
            // with some error.
            Ok(Err(_)) => self.initialize(LEGACY_VERSIONS[0], deadline).await,
            Err(_) => Err(self.closed_error()),
        }
    }

    async fn initialize(
        &self,
        version: &str,
        deadline: Deadline,
    ) -> Result<ServerDescription, McpError> {
        let result = self
            .request(
                "initialize",
                self.initialize_params(version),
                deadline,
                None,
            )
            .await?;
        self.legacy(&result)
    }

    fn modern(&self, result: &Value, requested: &str) -> Result<ServerDescription, McpError> {
        check_complete(result)?;
        let supported: Vec<String> = result
            .get("supportedVersions")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let version = if supported.is_empty() || supported.iter().any(|v| v == requested) {
            requested.to_owned()
        } else if let Some(version) = pick(&supported, MODERN_VERSIONS) {
            version.to_owned()
        } else {
            return Err(unsupported_versions(&supported));
        };
        let info = result
            .get("_meta")
            .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"));
        let description = describe(result, info, &version, Era::Modern);
        *lock(&self.shared.protocol) = Some((Era::Modern, version));
        Ok(description)
    }

    fn legacy(&self, result: &Value) -> Result<ServerDescription, McpError> {
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::Protocol("initialize result has no protocolVersion".into()))?;
        if !LEGACY_VERSIONS.contains(&version) {
            return Err(unsupported_versions(&[version.to_owned()]));
        }
        let description = describe(result, result.get("serverInfo"), version, Era::Legacy);
        *lock(&self.shared.protocol) = Some((Era::Legacy, version.to_owned()));
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(description)
    }

    /// All tools of the server, following pages. Tools that cannot be used
    /// are left out and described in the second list.
    pub async fn list_tools(
        &self,
        timeout: Duration,
    ) -> Result<(Vec<McpTool>, Vec<String>), McpError> {
        let deadline = Deadline::after(timeout);
        let mut tools = Vec::new();
        let mut problems = Vec::new();
        let mut names = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let mut params = Map::new();
            if let Some(cursor) = &cursor {
                params.insert("cursor".to_owned(), Value::String(cursor.clone()));
            }
            let result = self
                .request("tools/list", self.params(params), deadline, None)
                .await?;
            check_complete(&result)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Protocol("tools/list result has no tools".into()))?;
            for item in page {
                match parse_tool(item) {
                    Ok(tool) if !names.insert(tool.name.clone()) => {
                        problems.push(format!("tool \"{}\" is listed twice", tool.name));
                    }
                    Ok(tool) => tools.push(tool),
                    Err(problem) => problems.push(problem),
                }
                if tools.len() >= MAX_TOOLS {
                    problems.push(format!("only the first {MAX_TOOLS} tools are used"));
                    return Ok((tools, problems));
                }
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_owned()),
                _ => return Ok((tools, problems)),
            }
        }
        problems.push(format!(
            "the tool list was cut after {MAX_TOOL_PAGES} pages"
        ));
        Ok((tools, problems))
    }

    /// Calls a tool. `arguments` must be a JSON object.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<CallResult, McpError> {
        let deadline = Deadline::after(timeout);
        let mut request_state: Option<Value> = None;
        for _ in 0..=MAX_STATE_RETRIES {
            let mut params = Map::new();
            params.insert("name".to_owned(), Value::String(name.to_owned()));
            params.insert("arguments".to_owned(), arguments.clone());
            if let Some(state) = request_state.take() {
                params.insert("requestState".to_owned(), state);
            }
            let result = match self
                .request("tools/call", self.params(params), deadline, Some(cancel))
                .await
            {
                Err(McpError::Rpc(error)) if error.code == MISSING_CLIENT_CAPABILITY => {
                    return Err(missing_capability(&error));
                }
                other => other?,
            };
            match result.get("resultType").and_then(Value::as_str) {
                None | Some("complete") => return Ok(parse_call_result(&result)),
                Some("input_required") => {
                    let asks = result
                        .get("inputRequests")
                        .and_then(Value::as_object)
                        .is_some_and(|requests| !requests.is_empty());
                    if asks {
                        return Err(McpError::Unsupported(
                            "the tool asked for input (a question, a model reply or folders) that JARVIS cannot provide".into(),
                        ));
                    }
                    let Some(state) = result.get("requestState").cloned() else {
                        return Err(McpError::Protocol(
                            "input_required result without inputRequests or requestState".into(),
                        ));
                    };
                    request_state = Some(state);
                }
                Some(other) => {
                    return Err(McpError::Protocol(format!(
                        "unknown resultType \"{other}\""
                    )));
                }
            }
        }
        Err(McpError::Protocol(
            "the server kept asking to repeat the call".into(),
        ))
    }
}

async fn next(
    receiver: &mut Option<oneshot::Receiver<Reply>>,
) -> Result<Reply, oneshot::error::RecvError> {
    match receiver {
        Some(receiver) => receiver.await,
        None => std::future::pending().await,
    }
}

fn pick<'a>(offered: &[String], known: &[&'a str]) -> Option<&'a str> {
    known
        .iter()
        .find(|version| offered.iter().any(|v| v == *version))
        .copied()
}

fn unsupported_versions(offered: &[String]) -> McpError {
    let known: Vec<&str> = MODERN_VERSIONS
        .iter()
        .chain(LEGACY_VERSIONS)
        .copied()
        .collect();
    McpError::Unsupported(format!(
        "the server speaks MCP {}; JARVIS supports {}",
        if offered.is_empty() {
            "of an unknown version".to_owned()
        } else {
            offered.join(", ")
        },
        known.join(", ")
    ))
}

fn missing_capability(error: &RpcError) -> McpError {
    let needed: Vec<String> = error
        .data
        .as_ref()
        .and_then(|data| data.get("requiredCapabilities"))
        .and_then(Value::as_object)
        .map(|caps| caps.keys().cloned().collect())
        .unwrap_or_default();
    McpError::Unsupported(format!(
        "the tool needs client features JARVIS does not provide{}",
        if needed.is_empty() {
            String::new()
        } else {
            format!(" ({})", needed.join(", "))
        }
    ))
}

fn check_complete(result: &Value) -> Result<(), McpError> {
    match result.get("resultType").and_then(Value::as_str) {
        None | Some("complete") => Ok(()),
        Some(other) => Err(McpError::Protocol(format!(
            "unexpected resultType \"{other}\""
        ))),
    }
}

fn text_field(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| s.chars().take(max).collect::<String>())
        .filter(|s| !s.trim().is_empty())
}

fn describe(result: &Value, info: Option<&Value>, version: &str, era: Era) -> ServerDescription {
    let field = |name: &str, max: usize| text_field(info.and_then(|i| i.get(name)), max);
    ServerDescription {
        name: field("name", 128).unwrap_or_else(|| "unnamed server".to_owned()),
        version: field("version", 64).unwrap_or_default(),
        title: field("title", 128),
        protocol_version: version.to_owned(),
        era,
        instructions: text_field(result.get("instructions"), 4000),
        has_tools: result
            .get("capabilities")
            .and_then(|caps| caps.get("tools"))
            .is_some(),
    }
}

fn parse_rpc_error(error: &Value) -> RpcError {
    RpcError {
        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
        message: text_field(error.get("message"), 2000).unwrap_or_else(|| "unknown error".into()),
        data: error.get("data").cloned(),
    }
}

fn parse_tool(item: &Value) -> Result<McpTool, String> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "a tool without a name was skipped".to_owned())?;
    if name.is_empty()
        || name.chars().count() > MAX_TOOL_NAME
        || name.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(format!(
            "tool \"{}\" was skipped: invalid name",
            name.chars().take(40).collect::<String>()
        ));
    }
    let input_schema = item
        .get("inputSchema")
        .filter(|schema| schema.is_object())
        .cloned()
        .ok_or_else(|| format!("tool \"{name}\" was skipped: no input schema"))?;
    if input_schema.to_string().len() > MAX_SCHEMA_BYTES {
        return Err(format!(
            "tool \"{name}\" was skipped: its input schema is too large"
        ));
    }
    let annotations = item.get("annotations");
    let hint = |key: &str| {
        annotations
            .and_then(|a| a.get(key))
            .and_then(Value::as_bool)
    };
    Ok(McpTool {
        name: name.to_owned(),
        title: text_field(item.get("title"), 128)
            .or_else(|| text_field(annotations.and_then(|a| a.get("title")), 128)),
        description: text_field(item.get("description"), 8000).unwrap_or_default(),
        input_schema,
        hints: ToolHints {
            read_only: hint("readOnlyHint"),
            destructive: hint("destructiveHint"),
            idempotent: hint("idempotentHint"),
            open_world: hint("openWorldHint"),
        },
    })
}

fn decoded_size(data: Option<&Value>) -> usize {
    // Base64: four characters for three bytes.
    data.and_then(Value::as_str).map_or(0, |d| d.len() / 4 * 3)
}

fn parse_call_result(result: &Value) -> CallResult {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| {
                    let text = |key: &str| {
                        block
                            .get(key)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    match block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text" => Content::Text(text("text")),
                        "image" => Content::Image {
                            mime_type: text("mimeType"),
                            bytes: decoded_size(block.get("data")),
                        },
                        "audio" => Content::Audio {
                            mime_type: text("mimeType"),
                            bytes: decoded_size(block.get("data")),
                        },
                        "resource_link" => Content::ResourceLink {
                            uri: text("uri"),
                            name: text("name"),
                        },
                        "resource" => {
                            let resource = block.get("resource");
                            Content::Resource {
                                uri: resource
                                    .and_then(|r| r.get("uri"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                text: resource
                                    .and_then(|r| r.get("text"))
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            }
                        }
                        other => Content::Other(other.chars().take(32).collect()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    CallResult {
        content,
        structured: result.get("structuredContent").cloned(),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut incoming: mpsc::UnboundedReceiver<String>,
    shutdown: CancellationToken,
    shared: Arc<Shared>,
) {
    loop {
        let line = tokio::select! {
            biased;
            line = incoming.recv() => line,
            () = shutdown.cancelled() => None,
            () = shared.closed.cancelled() => None,
        };
        let Some(line) = line else {
            break;
        };
        let written = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        }
        .await;
        if let Err(err) = written {
            shared.close(format!("cannot write to the server: {err}"));
            break;
        }
    }
    // Dropping the writer closes the server's input.
    let _ = writer.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    reader: R,
    shared: Arc<Shared>,
    outgoing: mpsc::UnboundedSender<String>,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut line = Vec::new();
    let reason = loop {
        let read = tokio::select! {
            read = read_line(&mut reader, &mut line, MAX_MESSAGE_BYTES) => read,
            () = shared.closed.cancelled() => return,
        };
        match read {
            Ok(Some(false)) => {
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<Value>(&line) {
                    // Batches existed in 2025-03-26.
                    Ok(Value::Array(messages)) => {
                        for message in messages {
                            shared.dispatch(message, &outgoing);
                        }
                    }
                    Ok(message) => shared.dispatch(message, &outgoing),
                    // Some servers print other text to stdout; it is not a
                    // message, so it is skipped.
                    Err(err) => {
                        tracing::debug!(target: "jarvis_plugins::mcp", "skipped a line that is not JSON: {err}");
                    }
                }
            }
            Ok(Some(true)) => {
                break format!(
                    "the server sent a message larger than {} MB",
                    MAX_MESSAGE_BYTES / (1024 * 1024)
                );
            }
            Ok(None) => break "the server closed its output".to_owned(),
            Err(err) => break format!("cannot read from the server: {err}"),
        }
    };
    shared.close(reason);
}

/// Reads one line (without the line break) into `line`, keeping at most
/// `max` bytes. Returns `Some(true)` when the line was longer (the rest is
/// discarded) and `None` at the end of the stream.
pub(crate) async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut truncated = false;
    let mut read_any = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(read_any.then_some(truncated));
        }
        read_any = true;
        let (chunk, used, done) = match available.iter().position(|b| *b == b'\n') {
            Some(end) => (&available[..end], end + 1, true),
            None => (available, available.len(), false),
        };
        let room = max.saturating_sub(line.len());
        if chunk.len() > room {
            truncated = true;
        }
        line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        reader.consume(used);
        if done {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(truncated));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_split_and_capped() {
        let data = b"one\r\ntwo-is-long\nthree".to_vec();
        let mut reader = BufReader::with_capacity(4, &data[..]);
        let mut line = Vec::new();
        assert_eq!(
            read_line(&mut reader, &mut line, 8).await.ok(),
            Some(Some(false))
        );
        assert_eq!(line, b"one");
        assert_eq!(
            read_line(&mut reader, &mut line, 8).await.ok(),
            Some(Some(true))
        );
        assert_eq!(line, b"two-is-l");
        assert_eq!(
            read_line(&mut reader, &mut line, 8).await.ok(),
            Some(Some(false))
        );
        assert_eq!(line, b"three");
        assert_eq!(read_line(&mut reader, &mut line, 8).await.ok(), Some(None));
    }

    #[test]
    fn results_become_text() {
        let result = parse_call_result(&json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "resource_link", "uri": "file:///a.txt", "name": "a.txt"},
                {"type": "resource", "resource": {"uri": "file:///b.txt", "text": "body"}},
                {"type": "hologram"}
            ],
            "isError": true
        }));
        assert!(result.is_error);
        assert_eq!(
            result.to_text(),
            "hello\n[image image/png, 1 KB, not shown]\n[link a.txt: file:///a.txt]\n[resource file:///b.txt]\nbody\n[hologram content, not shown]"
        );
        let structured = parse_call_result(&json!({"content": [], "structuredContent": {"n": 1}}));
        assert_eq!(structured.to_text(), "{\"n\":1}");
    }

    #[test]
    fn tools_are_validated() {
        assert!(parse_tool(&json!({"name": "ok", "inputSchema": {"type": "object"}})).is_ok());
        assert!(parse_tool(&json!({"name": "no schema"})).is_err());
        assert!(parse_tool(&json!({"name": "bad name", "inputSchema": {}})).is_err());
        assert!(parse_tool(&json!({"name": "x".repeat(129), "inputSchema": {}})).is_err());
        let tool = parse_tool(&json!({
            "name": "t",
            "inputSchema": {},
            "annotations": {"title": "Nice", "readOnlyHint": true}
        }))
        .ok();
        assert_eq!(
            tool.as_ref().and_then(|t| t.title.clone()).as_deref(),
            Some("Nice")
        );
        assert_eq!(tool.and_then(|t| t.hints.read_only), Some(true));
    }
}
