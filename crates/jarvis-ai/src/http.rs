//! Shared HTTP plumbing for network providers: client construction, error
//! mapping, retries for idempotent requests and streaming decoders.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Duration;

use futures::stream::{self, BoxStream, StreamExt};
use serde::de::DeserializeOwned;
use url::Url;

use crate::{AiError, ChatChunk, ChatStream};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for short, non-streaming requests (health, model listing).
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_ATTEMPTS: u32 = 2;
const RETRY_BACKOFF: Duration = Duration::from_millis(300);
const MAX_ERROR_BODY_CHARS: usize = 300;
/// Upper bound for a single NDJSON line / SSE event, protects memory against
/// misbehaving servers.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Lazily built shared HTTP client. Building can only fail if the TLS backend
/// cannot be initialised; the error is reported on every use.
#[derive(Debug, Default)]
pub struct HttpClient {
    client: OnceLock<Result<reqwest::Client, String>>,
}

impl HttpClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Result<reqwest::Client, AiError> {
        self.client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    // Never route AI traffic through implicit proxies.
                    .no_proxy()
                    .user_agent(concat!("JARVIS/", env!("CARGO_PKG_VERSION")))
                    .pool_idle_timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|err| err.to_string())
            })
            .clone()
            .map_err(|err| AiError::Unavailable(format!("HTTP client unavailable: {err}")))
    }
}

/// Ensures the base URL ends with `/` so that `join` appends paths.
pub(crate) fn normalize_base(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

pub(crate) fn endpoint(base: &Url, path: &str) -> Result<Url, AiError> {
    base.join(path)
        .map_err(|err| AiError::InvalidRequest(format!("invalid endpoint {path}: {err}")))
}

pub(crate) fn map_transport_error(err: &reqwest::Error) -> AiError {
    if err.is_timeout() {
        AiError::Timeout
    } else if err.is_connect() {
        AiError::Unavailable(format!(
            "cannot connect to {}",
            err.url().map_or_else(|| "the server".to_owned(), origin)
        ))
    } else if err.is_decode() {
        AiError::Protocol(format!("unexpected response: {err}"))
    } else {
        AiError::Unavailable(err.to_string())
    }
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Reads an error response and extracts the server's message if possible.
pub(crate) async fn error_message(response: reqwest::Response) -> (reqwest::StatusCode, String) {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|json| {
            let error = json.get("error")?;
            error
                .as_str()
                .map(str::to_owned)
                .or_else(|| error.get("message")?.as_str().map(str::to_owned))
        })
        .unwrap_or(body);
    let message: String = message.chars().take(MAX_ERROR_BODY_CHARS).collect();
    let message = if message.trim().is_empty() {
        status.to_string()
    } else {
        format!("{status}: {}", message.trim())
    };
    (status, message)
}

/// Maps a non-success status of a request about `model` to an error.
pub(crate) async fn status_error(response: reqwest::Response, model: Option<&str>) -> AiError {
    let (status, message) = error_message(response).await;
    match status.as_u16() {
        404 if model.is_some() => AiError::ModelNotFound {
            model: model.unwrap_or_default().to_owned(),
        },
        400 | 422 => AiError::InvalidRequest(message),
        401 | 403 => AiError::Unavailable(format!("access denied by the server ({message})")),
        408 | 504 => AiError::Timeout,
        _ => AiError::Unavailable(message),
    }
}

/// Sends an idempotent request and decodes JSON. Retries once when the server
/// timed out or reported a temporary condition (502/503/504, e.g. a model
/// still loading). Refused connections are not retried: the server is not
/// running, and waiting would only delay the error.
pub(crate) async fn send_json<T, F>(build: F) -> Result<T, AiError>
where
    T: DeserializeOwned,
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut attempt = 1;
    loop {
        let (result, retryable) = match build().timeout(REQUEST_TIMEOUT).send().await {
            Err(err) => (Err(map_transport_error(&err)), err.is_timeout()),
            Ok(response) if !response.status().is_success() => {
                let retryable = matches!(response.status().as_u16(), 502..=504);
                (Err(status_error(response, None).await), retryable)
            }
            Ok(response) => (
                response
                    .json::<T>()
                    .await
                    .map_err(|err| AiError::Protocol(format!("unexpected response: {err}"))),
                false,
            ),
        };

        match result {
            Err(err) if retryable && attempt < RETRY_ATTEMPTS => {
                tracing::debug!(attempt, "retrying idempotent AI request: {err}");
                attempt += 1;
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
            other => return other,
        }
    }
}

/// Incremental decoder of a streamed response body into chat chunks.
pub(crate) trait ChunkDecoder: Send + 'static {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ChatChunk>, AiError>;
    /// Called once the body ends.
    fn finish(&mut self) -> Result<Vec<ChatChunk>, AiError>;
}

struct DecodeState<D> {
    body: BoxStream<'static, Result<Vec<u8>, AiError>>,
    decoder: D,
    queue: VecDeque<ChatChunk>,
    finished: bool,
}

/// Turns a streaming response into a [`ChatStream`]. The stream ends after
/// the first [`ChatChunk::Done`]; dropping it closes the connection.
pub(crate) fn decode_stream<D: ChunkDecoder>(
    response: reqwest::Response,
    decoder: D,
) -> ChatStream {
    let body = response
        .bytes_stream()
        .map(|item| {
            item.map(|bytes| bytes.to_vec())
                .map_err(|err| map_transport_error(&err))
        })
        .boxed();
    decode_body(body, decoder)
}

pub(crate) fn decode_body<D: ChunkDecoder>(
    body: BoxStream<'static, Result<Vec<u8>, AiError>>,
    decoder: D,
) -> ChatStream {
    let state = DecodeState {
        body,
        decoder,
        queue: VecDeque::new(),
        finished: false,
    };
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(chunk) = state.queue.pop_front() {
                if matches!(chunk, ChatChunk::Done { .. }) {
                    state.queue.clear();
                    state.finished = true;
                }
                return Some((Ok(chunk), state));
            }
            if state.finished {
                return None;
            }
            let decoded = match state.body.next().await {
                Some(Ok(bytes)) => state.decoder.feed(&bytes),
                Some(Err(err)) => Err(err),
                None => {
                    state.finished = true;
                    state.decoder.finish()
                }
            };
            match decoded {
                Ok(chunks) => state.queue.extend(chunks),
                Err(err) => {
                    state.finished = true;
                    state.queue.clear();
                    return Some((Err(err), state));
                }
            }
        }
    })
    .boxed()
}

/// Splits a byte stream into UTF-8 lines; tolerates `\r\n` and multi-byte
/// characters split across network chunks.
#[derive(Debug, Default)]
pub(crate) struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(into_utf8(line)?);
        }
        if self.pending.len() > MAX_LINE_BYTES {
            return Err(AiError::Protocol("response line too long".to_owned()));
        }
        Ok(lines)
    }

    pub(crate) fn finish(&mut self) -> Result<Option<String>, AiError> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        into_utf8(line).map(Some)
    }
}

fn into_utf8(bytes: Vec<u8>) -> Result<String, AiError> {
    String::from_utf8(bytes)
        .map_err(|_| AiError::Protocol("response is not valid UTF-8".to_owned()))
}

/// Minimal Server-Sent Events parser: returns the `data` payload of each
/// complete event. Other fields (`event`, `id`, `retry`) are ignored.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    lines: LineBuffer,
    data: Vec<String>,
}

impl SseParser {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        let mut events = Vec::new();
        for line in self.lines.push(bytes)? {
            self.handle_line(&line, &mut events);
        }
        Ok(events)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<String>, AiError> {
        let mut events = Vec::new();
        if let Some(line) = self.lines.finish()? {
            self.handle_line(&line, &mut events);
        }
        if !self.data.is_empty() {
            events.push(self.data.join("\n"));
            self.data.clear();
        }
        Ok(events)
    }

    fn handle_line(&mut self, line: &str, events: &mut Vec<String>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                events.push(self.data.join("\n"));
                self.data.clear();
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            self.data
                .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
        // Comments (":...") and other fields are ignored.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_buffer_handles_split_lines_and_utf8() {
        let mut buffer = LineBuffer::default();
        let text = "привіт\r\nworld\n".as_bytes();
        // Split inside the two-byte "п" and before "\n".
        assert!(buffer.push(&text[..1]).expect("push").is_empty());
        assert!(buffer.push(&text[1..8]).expect("push").is_empty());
        assert_eq!(buffer.push(&text[8..14]).expect("push"), vec!["привіт"]);
        assert_eq!(buffer.push(&text[14..]).expect("push"), vec!["world"]);
        assert_eq!(buffer.finish().expect("finish"), None);

        buffer.push(b"tail").expect("push");
        assert_eq!(buffer.finish().expect("finish"), Some("tail".to_owned()));
    }

    #[test]
    fn line_buffer_rejects_invalid_utf8() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.push(&[0xff, 0xfe, b'\n']).is_err());
    }

    #[test]
    fn sse_parser_collects_data_lines() {
        let mut parser = SseParser::default();
        let events = parser
            .push(b": keep-alive\n\ndata: {\"a\":1}\n\ndata: line1\ndata:line2\nevent: x\n\ndata: [DO")
            .expect("push");
        assert_eq!(events, vec!["{\"a\":1}", "line1\nline2"]);
        assert_eq!(parser.push(b"NE]\n\n").expect("push"), vec!["[DONE]"]);
        assert!(parser.finish().expect("finish").is_empty());
    }

    #[test]
    fn sse_parser_flushes_unterminated_event() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: last").expect("push").is_empty());
        assert_eq!(parser.finish().expect("finish"), vec!["last"]);
    }

    #[test]
    fn normalize_base_adds_trailing_slash() {
        let base = normalize_base(Url::parse("http://h:1/v1").expect("url"));
        assert_eq!(
            endpoint(&base, "models").expect("join").as_str(),
            "http://h:1/v1/models"
        );
        let root = normalize_base(Url::parse("http://h:1").expect("url"));
        assert_eq!(
            endpoint(&root, "api/tags").expect("join").as_str(),
            "http://h:1/api/tags"
        );
    }
}
