//! Minimal scripted HTTP/1.1 server for provider contract tests. Gives full
//! control over chunk boundaries, delays, hanging responses and observes
//! client disconnects (the cancellation contract).

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: String,
    pub authorization: Option<String>,
    /// Every header, names in lower case.
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum Reply {
    Json(u16, serde_json::Value),
    /// Chunked response; each chunk is written after its delay.
    Stream {
        content_type: &'static str,
        chunks: Vec<(Duration, Vec<u8>)>,
        /// Keep the connection open after the last chunk.
        hang: bool,
    },
}

impl Reply {
    pub fn stream(content_type: &'static str, body: &str, chunk_size: usize) -> Self {
        let chunks = body
            .as_bytes()
            .chunks(chunk_size.max(1))
            .map(|c| (Duration::ZERO, c.to_vec()))
            .collect();
        Reply::Stream {
            content_type,
            chunks,
            hang: false,
        }
    }
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

pub struct MockServer {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    disconnects: Arc<AtomicUsize>,
}

impl MockServer {
    pub async fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handler: Handler = Arc::new(handler);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let disconnects = Arc::new(AtomicUsize::new(0));

        let (reqs, discs) = (Arc::clone(&requests), Arc::clone(&disconnects));
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (handler, reqs, discs) =
                    (Arc::clone(&handler), Arc::clone(&reqs), Arc::clone(&discs));
                tokio::spawn(async move {
                    let _ = serve(socket, handler, reqs, discs).await;
                });
            }
        });

        Self {
            addr,
            requests,
            disconnects,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn last_request_to(&self, path: &str) -> Option<Request> {
        self.requests().into_iter().rev().find(|r| r.path == path)
    }

    /// Number of hanging responses whose client went away.
    pub fn disconnects(&self) -> usize {
        self.disconnects.load(Ordering::SeqCst)
    }

    pub async fn wait_for_disconnect(&self) -> bool {
        for _ in 0..100 {
            if self.disconnects() > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }
}

/// Returns an address on which nothing listens.
pub async fn unused_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    listener.local_addr().expect("addr")
}

async fn serve(
    socket: TcpStream,
    handler: Handler,
    requests: Arc<Mutex<Vec<Request>>>,
    disconnects: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();

    let mut content_length = 0usize;
    let mut authorization = None;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).await?;

    let request = Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
        authorization,
        headers,
    };
    requests.lock().unwrap().push(request.clone());
    let reply = handler(&request);

    let mut socket = reader.into_inner();
    match reply {
        Reply::Json(status, json) => {
            let body = json.to_string();
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
        }
        Reply::Stream {
            content_type,
            chunks,
            hang,
        } => {
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
            );
            socket.write_all(head.as_bytes()).await?;
            for (delay, chunk) in chunks {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await?;
                socket.write_all(&chunk).await?;
                socket.write_all(b"\r\n").await?;
                socket.flush().await?;
            }
            if hang {
                // Wait until the client closes the connection.
                let mut buf = [0u8; 64];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
                disconnects.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
            socket.write_all(b"0\r\n\r\n").await?;
        }
    }
    socket.flush().await?;
    socket.shutdown().await
}
