//! Web tools. `fetch_url` reaches only public hosts: every hop (including
//! redirects) is checked, the host is resolved and every address must be
//! public, and the request is pinned to the checked address so DNS
//! rebinding cannot redirect it to a local service.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jarvis_security::{ActionRequest, Capability, RiskLevel, check_ip, check_url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::{Host, Url};

use crate::util::{parse, to_args, truncate};
use crate::{PreparedCall, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

const MAX_REDIRECTS: usize = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(FetchUrl::new()), Arc::new(OpenUrl::new())]
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlArgs {
    url: String,
}

fn url_action(spec: &ToolSpec, summary: String, url: &Url) -> ActionRequest {
    ActionRequest {
        tool: spec.name.clone(),
        capabilities: spec.capabilities.clone(),
        risk: spec.risk,
        summary,
        paths: Vec::new(),
        program: None,
        url: Some(url.to_string()),
    }
}

#[derive(Debug)]
struct FetchUrl(ToolSpec);

impl FetchUrl {
    fn new() -> Self {
        Self(ToolSpec {
            name: "fetch_url".to_owned(),
            title: "Fetch a web page".to_owned(),
            description: "Downloads a public web page (http/https) and returns its text. Local and private network addresses are refused.".to_owned(),
            parameters: json!({"type": "object", "required": ["url"], "properties": {"url": {"type": "string"}}}),
            capabilities: vec![Capability::NetFetch],
            risk: RiskLevel::Medium,
            idempotent: true,
            plugin: None,
        })
    }
}

/// Resolves the URL's host and returns a public address to connect to.
async fn public_address(url: &Url) -> Result<SocketAddr, ToolError> {
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ToolError::InvalidInput("the URL has no port".to_owned()))?;
    let addresses: Vec<SocketAddr> = match url.host() {
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Domain(domain)) => tokio::net::lookup_host((domain, port))
            .await
            .map_err(|err| ToolError::Failed(format!("cannot resolve {domain}: {err}")))?
            .collect(),
        None => return Err(ToolError::InvalidInput("the URL has no host".to_owned())),
    };
    if addresses.is_empty() {
        return Err(ToolError::Failed("the host has no address".to_owned()));
    }
    for address in &addresses {
        check_ip(address.ip()).map_err(ToolError::Failed)?;
    }
    Ok(addresses[0])
}

#[async_trait]
impl Tool for FetchUrl {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: UrlArgs = parse(input)?;
        let url = check_url(args.url.trim()).map_err(ToolError::InvalidInput)?;
        Ok(PreparedCall {
            action: url_action(&self.0, format!("Fetch {url}"), &url),
            args: to_args(&UrlArgs {
                url: url.to_string(),
            })?,
        })
    }

    async fn run(&self, call: &PreparedCall, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: UrlArgs = parse(&call.args)?;
        let mut url = check_url(&args.url).map_err(ToolError::InvalidInput)?;
        for _ in 0..=MAX_REDIRECTS {
            let address = public_address(&url).await?;
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(FETCH_TIMEOUT)
                .user_agent(concat!("JARVIS/", env!("CARGO_PKG_VERSION")));
            if let Some(Host::Domain(domain)) = url.host() {
                builder = builder.resolve(domain, address);
            }
            let client = builder
                .build()
                .map_err(|err| ToolError::Failed(err.to_string()))?;
            let response = tokio::select! {
                response = client.get(url.clone()).send() => response.map_err(|err| ToolError::Failed(format!("request failed: {err}")))?,
                () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            };
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ToolError::Failed("redirect without a location".to_owned()))?;
                let next = url
                    .join(location)
                    .map_err(|err| ToolError::Failed(format!("invalid redirect: {err}")))?;
                url = check_url(next.as_str()).map_err(ToolError::Failed)?;
                continue;
            }
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_text = content_type.is_empty()
                || content_type.starts_with("text/")
                || content_type.contains("json")
                || content_type.contains("xml");
            if !is_text {
                return Err(ToolError::Failed(format!(
                    "{url} is not a text page ({content_type})"
                )));
            }
            let mut body = Vec::new();
            let mut response = response;
            loop {
                let chunk = tokio::select! {
                    chunk = response.chunk() => chunk.map_err(|err| ToolError::Failed(format!("download failed: {err}")))?,
                    () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                };
                let Some(chunk) = chunk else { break };
                body.extend_from_slice(&chunk);
                if body.len() > ctx.limits.max_fetch_bytes {
                    body.truncate(ctx.limits.max_fetch_bytes);
                    break;
                }
            }
            let raw = String::from_utf8_lossy(&body);
            let text = if content_type.contains("html") || raw.trim_start().starts_with('<') {
                html_to_text(&raw)
            } else {
                raw.into_owned()
            };
            return Ok(ToolOutput {
                summary: format!("Fetched {url} ({status})"),
                content: truncate(
                    &format!("URL: {url}\nStatus: {status}\n\n{text}"),
                    ctx.limits.max_output_chars,
                ),
            });
        }
        Err(ToolError::Failed(format!(
            "more than {MAX_REDIRECTS} redirects"
        )))
    }
}

/// Readable text of an HTML page: title, then text without scripts, styles
/// and tags, entities decoded, whitespace collapsed.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_lowercase();
    let title = lower.find("<title").and_then(|start| {
        let open_end = lower[start..].find('>')? + start + 1;
        let close = lower[open_end..].find("</title>")? + open_end;
        Some(decode_entities(html[open_end..close].trim()))
    });

    let mut text = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let bytes = lower.as_bytes();
    while i < html.len() {
        if bytes[i] == b'<' {
            // Skip whole blocks whose content is not text.
            let skipped = ["script", "style", "noscript", "svg", "head"]
                .iter()
                .find_map(|tag| {
                    let open = format!("<{tag}");
                    lower[i..].starts_with(&open).then(|| {
                        let close = format!("</{tag}>");
                        lower[i..]
                            .find(&close)
                            .map_or(html.len(), |end| i + end + close.len())
                    })
                });
            if let Some(next) = skipped {
                i = next;
                continue;
            }
            let end = lower[i..].find('>').map_or(html.len(), |e| i + e + 1);
            let tag = &lower[i..end];
            let block = [
                "<br",
                "</p",
                "</div",
                "</li",
                "</h1",
                "</h2",
                "</h3",
                "</h4",
                "</tr",
                "<hr",
                "</section",
                "</article",
            ]
            .iter()
            .any(|b| tag.starts_with(b));
            text.push(if block { '\n' } else { ' ' });
            i = end;
        } else {
            let next = lower[i..].find('<').map_or(html.len(), |n| i + n);
            text.push_str(&html[i..next]);
            i = next;
        }
    }
    let decoded = decode_entities(&text);
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            lines.push(line);
        }
    }
    match title {
        Some(title) if !title.is_empty() => format!("Title: {title}\n\n{}", lines.join("\n")),
        _ => lines.join("\n"),
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------

#[derive(Debug)]
struct OpenUrl(ToolSpec);

impl OpenUrl {
    fn new() -> Self {
        Self(ToolSpec {
            name: "open_url".to_owned(),
            title: "Open in the browser".to_owned(),
            description: "Opens a public http/https URL in the user's default browser.".to_owned(),
            parameters: json!({"type": "object", "required": ["url"], "properties": {"url": {"type": "string"}}}),
            capabilities: vec![Capability::OpenExternal],
            risk: RiskLevel::Medium,
            idempotent: false,
            plugin: None,
        })
    }
}

#[async_trait]
impl Tool for OpenUrl {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn prepare(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<PreparedCall, ToolError> {
        let args: UrlArgs = parse(input)?;
        let url = check_url(args.url.trim()).map_err(ToolError::InvalidInput)?;
        Ok(PreparedCall {
            action: url_action(&self.0, format!("Open {url} in the browser"), &url),
            args: to_args(&UrlArgs {
                url: url.to_string(),
            })?,
        })
    }

    async fn run(&self, call: &PreparedCall, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: UrlArgs = parse(&call.args)?;
        let url = check_url(&args.url).map_err(ToolError::InvalidInput)?;
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("rundll32.exe");
            command.arg("url.dll,FileProtocolHandler").arg(url.as_str());
            command
        } else if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("open");
            command.arg(url.as_str());
            command
        } else {
            let mut command = std::process::Command::new("xdg-open");
            command.arg(url.as_str());
            command
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
            .spawn()
            .map_err(|err| ToolError::io("opening the browser", err))?;
        Ok(ToolOutput {
            summary: format!("Opened {url}"),
            content: format!("Opened {url} in the default browser"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<html><head><title>Atlas &amp; Co</title><style>p{}</style></head>
            <body><script>alert("x")</script><h1>News</h1><p>Launch is on <b>Friday</b>&nbsp;&#8212; ok&#x21;</p>
            <ul><li>One</li><li>Two</li></ul></body></html>"#;
        let text = html_to_text(html);
        assert!(text.starts_with("Title: Atlas & Co"), "{text}");
        assert!(text.contains("News"));
        assert!(text.contains("Launch is on Friday — ok!"), "{text}");
        assert!(text.contains("One\nTwo"), "{text}");
        assert!(!text.contains("alert"));
        assert!(!text.contains("p{}"));
        assert_eq!(decode_entities("a &bogus; b & c"), "a &bogus; b & c");
    }

    #[tokio::test]
    async fn local_hosts_are_refused_after_resolution() {
        let url = Url::parse("http://localhost:9/").expect("url");
        assert!(public_address(&url).await.is_err());
        let url = Url::parse("http://127.0.0.1:9/").expect("url");
        assert!(public_address(&url).await.is_err());
    }
}
